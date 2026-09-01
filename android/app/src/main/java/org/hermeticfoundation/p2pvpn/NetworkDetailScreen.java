package org.hermeticfoundation.p2pvpn;

import android.app.Activity;
import android.view.LayoutInflater;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.Switch;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.List;
import java.util.StringJoiner;

final class NetworkDetailScreen {
    interface Listener {
        void back();

        void setNetworkEnabled(String networkId, boolean enabled);

        void openPairing();

        void approvePairing(String hostname);

        void rejectPairing();

        void copyPairingCode(CharSequence code);

        void removeNetwork(P2pVpnService.NetworkSnapshot network);
    }

    private final Activity activity;
    private final String networkId;
    private final Listener listener;
    private final TextView title;
    private final TextView state;
    private final Switch enabled;
    private final TextView hostname;
    private final TextView addresses;
    private final TextView peerId;
    private final Button openPairing;
    private final LinearLayout generatedGroup;
    private final TextView generatedCode;
    private final LinearLayout candidateGroup;
    private final TextView candidateDetails;
    private final EditText assignedHostname;
    private final Button approve;
    private final Button reject;
    private final TextView peersStatus;
    private final LinearLayout peers;
    private final TextView detailStatus;
    private final Button remove;
    private String displayedCandidatePeer;
    private String displayedPeerNetworkId;
    private long displayedPeerSnapshotTime = -1;
    private P2pVpnService.NetworkSnapshot displayedNetwork;

    NetworkDetailScreen(Activity activity, View root, String networkId, Listener listener) {
        this.activity = activity;
        this.networkId = networkId;
        this.listener = listener;
        title = root.findViewById(R.id.detail_title);
        state = root.findViewById(R.id.detail_state);
        enabled = root.findViewById(R.id.detail_enabled);
        hostname = root.findViewById(R.id.detail_hostname);
        addresses = root.findViewById(R.id.detail_addresses);
        peerId = root.findViewById(R.id.detail_peer_id);
        openPairing = root.findViewById(R.id.open_pairing);
        generatedGroup = root.findViewById(R.id.generated_code_group);
        generatedCode = root.findViewById(R.id.generated_code);
        candidateGroup = root.findViewById(R.id.candidate_group);
        candidateDetails = root.findViewById(R.id.candidate_details);
        assignedHostname = root.findViewById(R.id.assigned_hostname);
        approve = root.findViewById(R.id.approve_pairing);
        reject = root.findViewById(R.id.reject_pairing);
        peersStatus = root.findViewById(R.id.peers_status);
        peers = root.findViewById(R.id.peers);
        detailStatus = root.findViewById(R.id.detail_status);
        remove = root.findViewById(R.id.remove_network);

        root.findViewById(R.id.navigate_back).setOnClickListener(view -> listener.back());
        openPairing.setOnClickListener(view -> listener.openPairing());
        approve.setOnClickListener(
                view -> listener.approvePairing(assignedHostname.getText().toString()));
        reject.setOnClickListener(view -> listener.rejectPairing());
        root.findViewById(R.id.copy_code)
                .setOnClickListener(view -> listener.copyPairingCode(generatedCode.getText()));
        remove.setOnClickListener(
                view -> {
                    if (displayedNetwork != null) {
                        listener.removeNetwork(displayedNetwork);
                    }
                });
    }

    void render(
            P2pVpnService.Snapshot snapshot,
            boolean bound,
            String pendingPermissionNetworkId,
            String pendingMutationNetworkId,
            Boolean pendingMutationEnabled,
            String statusText) {
        if (snapshot == null) {
            title.setText(R.string.networks_title);
            enabled.setEnabled(false);
            detailStatus.setText(R.string.loading);
            return;
        }
        P2pVpnService.NetworkSnapshot network = findNetwork(snapshot);
        displayedNetwork = network;
        if (network == null) {
            return;
        }

        boolean desired =
                network.id.equals(pendingMutationNetworkId) && pendingMutationEnabled != null
                        ? pendingMutationEnabled
                        : network.enabled;
        NetworkUiState uiState =
                NetworkUiState.from(
                        desired, network.phase, network.detail, snapshot.connectionRequested);
        boolean canMutate =
                bound
                        && !snapshot.busy
                        && !snapshot.pairingActive
                        && pendingPermissionNetworkId == null
                        && pendingMutationNetworkId == null;

        title.setText(network.name);
        state.setText(NetworkUiText.display(activity, uiState));
        enabled.setOnCheckedChangeListener(null);
        enabled.setChecked(desired);
        enabled.setEnabled(canMutate);
        enabled.setContentDescription(
                activity.getString(
                        desired ? R.string.disable_network_named : R.string.enable_network_named,
                        network.name));
        enabled.setOnCheckedChangeListener(
                (button, checked) -> {
                    if (checked != desired) {
                        listener.setNetworkEnabled(network.id, checked);
                    }
                });
        hostname.setText(network.hostname);
        addresses.setText(String.join("\n", network.addresses));
        peerId.setText(network.peerId);

        boolean selectedAvailable =
                network.selected
                        && network.enabled
                        && "running".equals(network.phase)
                        && snapshot.connected;
        openPairing.setEnabled(bound && selectedAvailable && !snapshot.busy);

        boolean hasCode =
                network.selected
                        && snapshot.pairingCode != null
                        && !snapshot.pairingCode.isEmpty();
        generatedGroup.setVisibility(hasCode ? View.VISIBLE : View.GONE);
        generatedCode.setText(hasCode ? snapshot.pairingCode : "");

        boolean hasCandidate = network.selected && snapshot.candidatePeer != null;
        candidateGroup.setVisibility(hasCandidate ? View.VISIBLE : View.GONE);
        approve.setEnabled(bound && hasCandidate && snapshot.connected);
        reject.setEnabled(bound && hasCandidate && snapshot.connected);
        renderCandidate(snapshot, hasCandidate);

        renderPeers(network);
        detailStatus.setText(statusText);
        remove.setEnabled(bound && !snapshot.busy && !snapshot.pairingActive);
    }

    private void renderPeers(P2pVpnService.NetworkSnapshot network) {
        PeerSnapshot snapshot = network.peers;
        if (snapshot == null) {
            clearPeerRows();
            if (!network.enabled) {
                peersStatus.setText(R.string.peers_enable_network);
            } else if ("starting".equals(network.phase)) {
                peersStatus.setText(R.string.peers_loading);
            } else {
                peersStatus.setText(R.string.peers_unavailable);
            }
            return;
        }

        if (snapshot.truncated) {
            peersStatus.setText(
                    activity.getString(
                            R.string.peers_truncated,
                            snapshot.returnedPeers,
                            snapshot.totalPeers));
        } else {
            peersStatus.setText(
                    activity.getResources()
                            .getQuantityString(
                                    R.plurals.peer_count,
                                    snapshot.returnedPeers,
                                    snapshot.returnedPeers));
        }
        if (network.id.equals(displayedPeerNetworkId)
                && snapshot.observedAtUnixSeconds == displayedPeerSnapshotTime) {
            return;
        }

        peers.removeAllViews();
        displayedPeerNetworkId = network.id;
        displayedPeerSnapshotTime = snapshot.observedAtUnixSeconds;
        LayoutInflater inflater = LayoutInflater.from(activity);
        for (PeerSnapshot.Peer peer : snapshot.peers) {
            View row = inflater.inflate(R.layout.row_peer, peers, false);
            ((TextView) row.findViewById(R.id.peer_name)).setText(peerName(peer));
            ((TextView) row.findViewById(R.id.peer_state)).setText(peerState(peer));
            ((TextView) row.findViewById(R.id.peer_addresses)).setText(peerAddresses(peer));
            ((TextView) row.findViewById(R.id.peer_membership))
                    .setText(peerMembership(peer));
            ((TextView) row.findViewById(R.id.peer_identity)).setText(peer.peerId);
            peers.addView(row);
        }
    }

    private void clearPeerRows() {
        if (displayedPeerNetworkId != null || peers.getChildCount() > 0) {
            peers.removeAllViews();
        }
        displayedPeerNetworkId = null;
        displayedPeerSnapshotTime = -1;
    }

    private String peerName(PeerSnapshot.Peer peer) {
        if (!peer.hostnames.isEmpty()) {
            return String.join(", ", peer.hostnames);
        }
        int prefixLength = Math.min(16, peer.peerId.length());
        return peer.peerId.substring(0, prefixLength)
                + (prefixLength < peer.peerId.length() ? "..." : "");
    }

    private String peerState(PeerSnapshot.Peer peer) {
        String state;
        switch (peer.connectionState) {
            case LOCAL:
                return activity.getString(R.string.peer_state_local);
            case CONNECTED:
                state = activity.getString(R.string.peer_state_connected);
                break;
            case CONNECTING:
                state = activity.getString(R.string.peer_state_connecting);
                break;
            case RECOVERING:
                state = activity.getString(R.string.peer_state_recovering);
                break;
            case DISCONNECTED:
            default:
                state = activity.getString(R.string.peer_state_disconnected);
                break;
        }
        if (peer.selectedPath.isEmpty() || peer.pathOrigin.isEmpty()) {
            return state;
        }
        return activity.getString(
                R.string.peer_state_path,
                state,
                pathName(peer.selectedPath.get()),
                pathOriginName(peer.pathOrigin.get()));
    }

    private String peerAddresses(PeerSnapshot.Peer peer) {
        List<String> addresses = new ArrayList<>(peer.ipv4.size() + peer.ipv6.size());
        addresses.addAll(peer.ipv4);
        addresses.addAll(peer.ipv6);
        return String.join("\n", addresses);
    }

    private String peerMembership(PeerSnapshot.Peer peer) {
        StringJoiner sources = new StringJoiner(", ");
        for (PeerSnapshot.MembershipSource source : peer.membershipSources) {
            switch (source) {
                case LOCAL_CONFIGURATION:
                    sources.add(activity.getString(R.string.peer_membership_local));
                    break;
                case PEER_CONFIGURATION:
                    sources.add(activity.getString(R.string.peer_membership_configured));
                    break;
                case SIGNED_MEMBERSHIP:
                    sources.add(activity.getString(R.string.peer_membership_signed));
                    break;
            }
        }
        return activity.getString(R.string.peer_membership, sources.toString());
    }

    private String pathName(PeerSnapshot.PathKind path) {
        switch (path) {
            case DIRECT_UDP_DATAGRAM:
                return activity.getString(R.string.peer_path_udp);
            case DIRECT_QUIC_DATAGRAM:
                return activity.getString(R.string.peer_path_quic_datagram);
            case DIRECT_QUIC_STREAM:
                return activity.getString(R.string.peer_path_quic_stream);
            case DIRECT_TCP_STREAM:
                return activity.getString(R.string.peer_path_tcp_stream);
            case CIRCUIT_RELAY:
            default:
                return activity.getString(R.string.peer_path_relay);
        }
    }

    private String pathOriginName(PeerSnapshot.PathOrigin origin) {
        switch (origin) {
            case CONFIGURED:
                return activity.getString(R.string.peer_origin_configured);
            case MDNS:
                return activity.getString(R.string.peer_origin_mdns);
            case KADEMLIA:
                return activity.getString(R.string.peer_origin_kademlia);
            case IDENTIFY:
                return activity.getString(R.string.peer_origin_identify);
            case RELAY_CIRCUIT:
                return activity.getString(R.string.peer_origin_relay);
            case DCUTR:
                return activity.getString(R.string.peer_origin_dcutr);
            case PACKET_PLANE_NEGOTIATION:
                return activity.getString(R.string.peer_origin_packet_plane);
            case UNKNOWN:
            default:
                return activity.getString(R.string.peer_origin_unknown);
        }
    }

    private void renderCandidate(P2pVpnService.Snapshot snapshot, boolean hasCandidate) {
        if (!hasCandidate) {
            displayedCandidatePeer = null;
            candidateDetails.setText("");
            return;
        }
        StringBuilder details = new StringBuilder(snapshot.candidatePeer);
        if (snapshot.candidateFingerprint != null) {
            details.append("\n").append(snapshot.candidateFingerprint);
        }
        if (snapshot.candidateVpnIp != null) {
            details.append("\n")
                    .append(activity.getString(R.string.requested_ip, snapshot.candidateVpnIp));
        }
        candidateDetails.setText(details.toString());
        if (!snapshot.candidatePeer.equals(displayedCandidatePeer)) {
            displayedCandidatePeer = snapshot.candidatePeer;
            assignedHostname.setText(
                    snapshot.candidateHostname == null ? "" : snapshot.candidateHostname);
        }
    }

    private P2pVpnService.NetworkSnapshot findNetwork(P2pVpnService.Snapshot snapshot) {
        for (P2pVpnService.NetworkSnapshot network : snapshot.networks) {
            if (network.id.equals(networkId)) {
                return network;
            }
        }
        return null;
    }
}
