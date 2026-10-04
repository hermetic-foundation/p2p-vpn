//! Only installed checkpoint authority may retire legacy enrollment proofs.

use super::*;
use crate::membership::checkpoint::CheckpointMember;

impl CodePairingSessions {
    pub(crate) fn retire_checkpoint_artifacts_with<E: From<CodePairingSessionError>>(
        &mut self,
        network: &str,
        members: &[CheckpointMember],
        wall_now: u64,
        persist: impl FnOnce(&[u8]) -> Result<(), E>,
        mut read_back: impl FnMut() -> Result<Option<Vec<u8>>, E>,
    ) -> Result<bool, E> {
        if self.checkpoint_completion_uncertain() {
            return Err(CodePairingSessionError::Conflict.into());
        }
        if self.open.is_none()
            && self.join.is_none()
            && self.enrollments.is_empty()
            && self.receipts.is_empty()
            && self.replay_tokens.is_empty()
            && self.pending_approval.is_none()
            && self.inbound_ticket.is_none()
            && self.retained_inbound_tickets.is_empty()
        {
            return Ok(false);
        }
        let previous = self.encode_persisted(network)?;
        let (next, retired) = plan_retirement(self, network, members, wall_now)?;
        let completed = serde_json::to_vec_pretty(&next).map_err(CodePairingSessionError::from)?;
        if previous == completed {
            return Ok(false);
        }
        // In-memory expiry/discovery changes need not match the last saved bytes.
        // Compare a failed write against the actual durable baseline, not that view.
        let durable_previous = read_back()?;
        if let Err(error) = persist(&completed) {
            if !matches!(read_back(), Ok(visible) if visible == durable_previous) {
                // Share the existing durability gate: ordinary saves cannot restore old
                // proof copies before the visible replacement is reconciled and synced.
                self.uncertain_checkpoint_completion = Some(UncertainCheckpointCompletion {
                    previous: durable_previous.unwrap_or(previous),
                    completed,
                });
            }
            return Err(error);
        }
        // Preserve live PAKE sessions, requests and operation timers for unrelated owners.
        for operation in &retired {
            self.clear_transient_handshakes(operation);
        }
        if self
            .join_lookup
            .as_ref()
            .is_some_and(|(operation, _)| retired.contains(operation))
        {
            self.join_lookup = None;
        }
        if next.open.is_none() {
            self.open = None;
        }
        if next.join.is_none() {
            self.join = None;
        }
        if next.pending_approval.is_none() {
            self.pending_approval = None;
        }
        if next.inbound_ticket.is_none() {
            self.inbound_ticket = None;
        }
        self.retained_inbound_tickets.retain(|ticket| {
            next.retained_inbound_tickets
                .iter()
                .any(|keep| keep.ticket == ticket.ticket)
        });
        self.enrollments = next.enrollments;
        self.receipts = next.receipts;
        self.replay_tokens = next.replay_tokens;
        Ok(true)
    }
}

fn plan_retirement(
    sessions: &CodePairingSessions,
    network: &str,
    members: &[CheckpointMember],
    wall_now: u64,
) -> Result<(PersistedCodePairingSessions, HashSet<String>), CodePairingSessionError> {
    let mut next = PersistedCodePairingSessions::from_runtime(sessions, network);
    let protected = sessions
        .enrollments
        .iter()
        .filter(|entry| entry.state != PairingEnrollmentState::Applied)
        .map(|entry| entry.operation_id.clone())
        .collect::<HashSet<_>>();
    let mut retired = HashSet::new();
    let mut replay = Vec::new();
    let obsolete = |response: &PairingResponse| obsolete_response(response, members, wall_now);
    next.enrollments.retain(|entry| {
        if entry.state == PairingEnrollmentState::Applied && obsolete(&entry.response) {
            retired.insert(entry.operation_id.clone());
            replay.push((
                entry.response.payload.rendezvous_token.clone(),
                response_deadline(&entry.response),
            ));
            false
        } else {
            true
        }
    });
    if let Some(open) = &next.open
        && !protected.contains(&open.id)
        && (open.completed.as_ref().is_some_and(obsolete)
            || (open.terminal.is_some() && wall_now > open.expires_at_unix_seconds))
    {
        if let Some(response) = &open.completed {
            replay.push((
                response.payload.rendezvous_token.clone(),
                response_deadline(response),
            ));
        }
        retired.insert(open.id.clone());
        next.open = None;
    }
    if let Some(join) = &next.join
        && !protected.contains(&join.id)
        && (join
            .completed
            .as_ref()
            .is_some_and(|(_, response)| obsolete(response))
            || (join.terminal.is_some() && wall_now > join.expires_at_unix_seconds))
    {
        if let Some((_, response)) = &join.completed {
            replay.push((
                response.payload.rendezvous_token.clone(),
                response_deadline(response),
            ));
        }
        retired.insert(join.id.clone());
        next.join = None;
    }
    let mut keep_ticket = |ticket: &PersistedInboundTicket| {
        if protected.contains(&ticket.operation_id) {
            return true;
        }
        let remove = retired.contains(&ticket.operation_id)
            || match &ticket.outcome {
                InboundTicketOutcome::Accepted(response) => obsolete(response),
                InboundTicketOutcome::Rejected(_) => wall_now > ticket.expires_at_unix_seconds,
                InboundTicketOutcome::Pending => false,
            };
        if remove && let InboundTicketOutcome::Accepted(response) = &ticket.outcome {
            replay.push((
                response.payload.rendezvous_token.clone(),
                response_deadline(response),
            ));
        }
        !remove
    };
    next.inbound_ticket = next.inbound_ticket.filter(&mut keep_ticket);
    next.retained_inbound_tickets.retain(keep_ticket);
    if next
        .pending_approval
        .as_ref()
        .is_some_and(|approval| retired.contains(&approval.operation_id))
    {
        next.pending_approval = None;
    }
    compact_guards(&mut next, members, wall_now, &retired, replay)?;
    Ok((next, retired))
}

fn compact_guards(
    next: &mut PersistedCodePairingSessions,
    members: &[CheckpointMember],
    wall_now: u64,
    retired: &HashSet<String>,
    replay: Vec<(String, u64)>,
) -> Result<(), CodePairingSessionError> {
    let active = |peer: &str| {
        members
            .iter()
            .any(|member| member.subject.peer_id == peer && member.active_at(wall_now))
    };
    for receipt in &mut next.receipts {
        receipt.expires_at_unix_seconds = receipt.expires_at_unix_seconds.min(
            receipt
                .completed_at_unix_seconds
                .saturating_add(MAX_CODE_PAIRING_EXPIRES_IN_SECONDS),
        );
    }
    next.receipts.retain(|receipt| {
        wall_now <= receipt.expires_at_unix_seconds
            && !retired.contains(&receipt.operation_id)
            && active(&receipt.local_peer)
            && active(&receipt.remote_peer)
    });
    // Clamp old long-TTL guards once, without extending them during maintenance/restarts.
    for token in &mut next.replay_tokens {
        token.expires_at_unix_seconds = token
            .expires_at_unix_seconds
            .min(wall_now.saturating_add(MAX_CODE_PAIRING_EXPIRES_IN_SECONDS));
    }
    next.replay_tokens
        .retain(|token| wall_now <= token.expires_at_unix_seconds);
    for (rendezvous_token, expires) in replay {
        if wall_now > expires {
            continue;
        }
        if let Some(token) = next
            .replay_tokens
            .iter_mut()
            .find(|token| token.token == rendezvous_token)
        {
            token.expires_at_unix_seconds = token.expires_at_unix_seconds.max(expires);
        } else {
            if next.replay_tokens.len() >= MAX_CODE_PAIRING_REPLAY_TOKENS {
                return Err(CodePairingSessionError::Capacity);
            }
            next.replay_tokens.push(PairingReplayToken {
                token: rendezvous_token,
                expires_at_unix_seconds: expires,
                aborted_operation_id: None,
            });
        }
    }
    Ok(())
}

fn response_deadline(response: &PairingResponse) -> u64 {
    response.payload.expires_at_unix_seconds.min(
        response
            .payload
            .issued_at_unix_seconds
            .saturating_add(MAX_CODE_PAIRING_EXPIRES_IN_SECONDS),
    )
}

pub(super) fn obsolete_response(
    response: &PairingResponse,
    members: &[CheckpointMember],
    now: u64,
) -> bool {
    let Some(grant) = &response.payload.checkpoint else {
        return true;
    };
    now > response_deadline(response)
        || [&grant.inviter, &grant.joiner].iter().any(|granted| {
            !members.iter().any(|selected| {
                selected.subject == granted.subject
                    && selected.incarnation == granted.incarnation
                    && selected.active_at(now)
            })
        })
}
