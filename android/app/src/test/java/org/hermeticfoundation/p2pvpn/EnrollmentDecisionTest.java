package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public final class EnrollmentDecisionTest {
    @Test
    public void artifactsAlwaysTakePriority() {
        assertEquals(
                EnrollmentDecision.Action.APPLY_ARTIFACTS,
                EnrollmentDecision.evaluate("completed", true, false));
    }

    @Test
    public void inviterCandidateRequiresExplicitApproval() {
        assertEquals(
                EnrollmentDecision.Action.AWAIT_APPROVAL,
                EnrollmentDecision.evaluate("awaiting_approval", false, true));
    }

    @Test
    public void discoveryContinuesPolling() {
        assertEquals(
                EnrollmentDecision.Action.POLL,
                EnrollmentDecision.evaluate("discovering", false, false));
    }

    @Test
    public void failuresAreTerminal() {
        assertEquals(
                EnrollmentDecision.Action.TERMINAL,
                EnrollmentDecision.evaluate("failed", false, false));
    }
}
