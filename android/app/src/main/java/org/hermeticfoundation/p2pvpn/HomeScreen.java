package org.hermeticfoundation.p2pvpn;

import android.app.Activity;
import android.view.LayoutInflater;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.PopupMenu;
import android.widget.Switch;
import android.widget.TextView;

final class HomeScreen {
    interface Listener {
        void addNetwork();

        void exportDiagnostics();

        void resetUnreadableProfile();

        void openNetwork(String networkId);

        void setNetworkEnabled(String networkId, boolean enabled);
    }

    private final Activity activity;
    private final Listener listener;
    private final LinearLayout networks;
    private final TextView empty;
    private final LinearLayout recovery;
    private final Button reset;
    private final Button add;
    private final View overflow;
    private final TextView status;

    HomeScreen(Activity activity, View root, Listener listener) {
        this.activity = activity;
        this.listener = listener;
        networks = root.findViewById(R.id.networks);
        empty = root.findViewById(R.id.empty_networks);
        recovery = root.findViewById(R.id.profile_recovery);
        reset = root.findViewById(R.id.reset_profile);
        add = root.findViewById(R.id.add_network);
        overflow = root.findViewById(R.id.home_overflow);
        status = root.findViewById(R.id.home_status);

        add.setOnClickListener(view -> listener.addNetwork());
        overflow.setOnClickListener(this::showOverflowMenu);
        reset.setOnClickListener(view -> listener.resetUnreadableProfile());
    }

    private void showOverflowMenu(View anchor) {
        PopupMenu menu = new PopupMenu(activity, anchor);
        menu.inflate(R.menu.home_overflow);
        menu.setOnMenuItemClickListener(
                item -> {
                    if (item.getItemId() != R.id.action_export_diagnostics) {
                        return false;
                    }
                    listener.exportDiagnostics();
                    return true;
                });
        menu.show();
    }

    void render(
            P2pVpnService.Snapshot snapshot,
            boolean bound,
            String pendingPermissionNetworkId,
            String pendingMutationNetworkId,
            Boolean pendingMutationEnabled,
            String statusText) {
        networks.removeAllViews();
        if (snapshot == null) {
            empty.setVisibility(View.VISIBLE);
            recovery.setVisibility(View.GONE);
            add.setEnabled(false);
            overflow.setEnabled(false);
            status.setText(R.string.loading);
            return;
        }

        boolean hasNetworks = !snapshot.networks.isEmpty();
        empty.setVisibility(hasNetworks ? View.GONE : View.VISIBLE);
        recovery.setVisibility(snapshot.profileUnreadable ? View.VISIBLE : View.GONE);
        reset.setEnabled(
                bound
                        && snapshot.profileUnreadable
                        && !snapshot.connectionRequested
                        && !snapshot.busy);
        add.setEnabled(
                bound
                        && !snapshot.profileUnreadable
                        && !snapshot.busy
                        && !snapshot.pairingActive
                        && snapshot.networks.size() < ProfileCollection.MAX_NETWORKS);
        overflow.setEnabled(bound);

        boolean canMutate =
                bound
                        && !snapshot.busy
                        && !snapshot.pairingActive
                        && pendingPermissionNetworkId == null
                        && pendingMutationNetworkId == null;
        LayoutInflater inflater = LayoutInflater.from(activity);
        for (P2pVpnService.NetworkSnapshot network : snapshot.networks) {
            View row = inflater.inflate(R.layout.network_row, networks, false);
            TextView name = row.findViewById(R.id.network_name);
            TextView hostname = row.findViewById(R.id.network_hostname);
            TextView state = row.findViewById(R.id.network_state);
            Switch enabled = row.findViewById(R.id.network_enabled);
            boolean desired =
                    network.id.equals(pendingMutationNetworkId)
                                    && pendingMutationEnabled != null
                            ? pendingMutationEnabled
                            : network.enabled;
            NetworkUiState uiState =
                    NetworkUiState.from(
                            desired,
                            network.phase,
                            network.detail,
                            snapshot.connectionRequested);

            name.setText(network.name);
            hostname.setText(network.hostname);
            state.setText(NetworkUiText.display(activity, uiState));
            enabled.setChecked(desired);
            enabled.setEnabled(canMutate);
            enabled.setContentDescription(
                    activity.getString(
                            desired
                                    ? R.string.disable_network_named
                                    : R.string.enable_network_named,
                            network.name));
            enabled.setOnCheckedChangeListener(
                    (button, checked) -> {
                        if (checked != desired) {
                            listener.setNetworkEnabled(network.id, checked);
                        }
                    });
            row.setOnClickListener(view -> listener.openNetwork(network.id));
            networks.addView(row);
        }
        status.setText(statusText);
    }
}
