package org.hermeticfoundation.p2pvpn;

import android.app.Activity;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;

final class AddNetworkScreen {
    interface Listener {
        void back();

        void showCreate();

        void showJoin();

        void createNetwork(String networkName);

        void joinNetwork(String pairingCode);
    }

    private final Activity activity;
    private final AppNavigation.Screen mode;
    private final TextView title;
    private final View choice;
    private final View createForm;
    private final View joinForm;
    private final EditText networkName;
    private final EditText joinCode;
    private final Button create;
    private final Button join;
    private final View showCreate;
    private final View showJoin;
    private final TextView status;

    AddNetworkScreen(
            Activity activity, View root, AppNavigation.Screen mode, Listener listener) {
        this.activity = activity;
        this.mode = mode;
        title = root.findViewById(R.id.add_screen_title);
        choice = root.findViewById(R.id.add_choice);
        createForm = root.findViewById(R.id.create_form);
        joinForm = root.findViewById(R.id.join_form);
        networkName = root.findViewById(R.id.network_name_input);
        joinCode = root.findViewById(R.id.join_code_input);
        create = root.findViewById(R.id.create_network);
        join = root.findViewById(R.id.join_network);
        showCreate = root.findViewById(R.id.show_create_network);
        showJoin = root.findViewById(R.id.show_join_network);
        status = root.findViewById(R.id.add_status);

        root.findViewById(R.id.navigate_back).setOnClickListener(view -> listener.back());
        showCreate.setOnClickListener(view -> listener.showCreate());
        showJoin.setOnClickListener(view -> listener.showJoin());
        create.setOnClickListener(
                view -> {
                    String name = networkName.getText().toString().trim();
                    if (name.isEmpty()) {
                        networkName.setError(activity.getString(R.string.network_name_hint));
                        return;
                    }
                    listener.createNetwork(name);
                });
        networkName.setOnEditorActionListener(
                (view, actionId, event) -> {
                    if (actionId == android.view.inputmethod.EditorInfo.IME_ACTION_DONE) {
                        create.performClick();
                        return true;
                    }
                    return false;
                });
        join.setOnClickListener(
                view -> {
                    String code = joinCode.getText().toString().trim();
                    if (code.isEmpty()) {
                        joinCode.setError(activity.getString(R.string.pairing_code_hint));
                        return;
                    }
                    listener.joinNetwork(code);
                });
        joinCode.setOnEditorActionListener(
                (view, actionId, event) -> {
                    if (actionId == android.view.inputmethod.EditorInfo.IME_ACTION_DONE) {
                        join.performClick();
                        return true;
                    }
                    return false;
                });
    }

    void render(P2pVpnService.Snapshot snapshot, boolean bound, boolean creationPending, String statusText) {
        boolean choosing = mode == AppNavigation.Screen.ADD;
        boolean creating = mode == AppNavigation.Screen.CREATE;
        boolean joining = mode == AppNavigation.Screen.JOIN;
        title.setText(
                creating
                        ? R.string.create_network
                        : joining ? R.string.join_by_code : R.string.add_network);
        choice.setVisibility(choosing ? View.VISIBLE : View.GONE);
        createForm.setVisibility(creating ? View.VISIBLE : View.GONE);
        joinForm.setVisibility(joining ? View.VISIBLE : View.GONE);

        boolean available =
                bound
                        && snapshot != null
                        && !snapshot.profileUnreadable
                        && !snapshot.busy
                        && !snapshot.pairingActive;
        boolean hasCapacity =
                snapshot != null && snapshot.networks.size() < ProfileCollection.MAX_NETWORKS;
        showCreate.setEnabled(available && hasCapacity);
        showJoin.setEnabled(available);
        create.setEnabled(available && hasCapacity && !creationPending);
        join.setEnabled(available);
        if (snapshot == null) {
            status.setText(R.string.loading);
        } else {
            status.setText(statusText);
        }
    }
}
