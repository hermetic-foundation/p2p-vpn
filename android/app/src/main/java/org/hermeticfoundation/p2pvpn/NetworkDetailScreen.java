package org.hermeticfoundation.p2pvpn;

import android.app.Activity;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.Switch;
import android.widget.TextView;

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
    private final TextView detailStatus;
    private final Button remove;
    private String displayedCandidatePeer;
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

        peersStatus.setText(
                snapshot.peerDetail == null || snapshot.peerDetail.isEmpty()
                        ? activity.getString(R.string.peers_unavailable)
                        : snapshot.peerDetail);
        detailStatus.setText(statusText);
        remove.setEnabled(bound && !snapshot.busy && !snapshot.pairingActive);
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
