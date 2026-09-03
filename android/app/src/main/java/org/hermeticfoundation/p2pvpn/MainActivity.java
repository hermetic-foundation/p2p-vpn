package org.hermeticfoundation.p2pvpn;

import android.Manifest;
import android.annotation.SuppressLint;
import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ClipDescription;
import android.content.ClipboardManager;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.net.VpnService;
import android.os.Build;
import android.os.Bundle;
import android.os.IBinder;
import android.os.PersistableBundle;
import android.view.LayoutInflater;
import android.view.View;
import android.widget.FrameLayout;
import android.widget.Toast;
import android.window.OnBackInvokedCallback;
import android.window.OnBackInvokedDispatcher;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

public final class MainActivity extends Activity implements P2pVpnService.Listener {
    private static final int VPN_PERMISSION_REQUEST = 100;
    private static final int NOTIFICATION_PERMISSION_REQUEST = 101;
    private static final int DIAGNOSTIC_EXPORT_REQUEST = 102;
    private static final int LOCAL_NETWORK_PERMISSION_REQUEST = 103;

    private static final String STATE_DIAGNOSTIC_REPORT = "pending_diagnostic_report";
    private static final String STATE_NAVIGATION_SCREEN = "navigation_screen";
    private static final String STATE_NAVIGATION_NETWORK = "navigation_network";
    private static final String STATE_PENDING_ENABLE_NETWORK = "pending_enable_network";
    private static final String STATE_PENDING_MUTATION_NETWORK = "pending_mutation_network";
    private static final String STATE_PENDING_MUTATION_ENABLED = "pending_mutation_enabled";
    private static final String STATE_PENDING_MUTATION_PRESENT = "pending_mutation_present";
    private static final String STATE_MUTATION_OBSERVED_BUSY = "mutation_observed_busy";
    private static final String STATE_MUTATION_IDLE_OBSERVATIONS = "mutation_idle_observations";
    private static final String STATE_CREATION_PENDING = "creation_pending";
    private static final String STATE_CREATION_OBSERVED_BUSY = "creation_observed_busy";
    private static final String STATE_CREATION_NETWORK_COUNT = "creation_network_count";
    private static final String STATE_CREATION_IDLE_OBSERVATIONS = "creation_idle_observations";

    private FrameLayout screenContainer;
    private AppNavigation navigation = AppNavigation.home();
    private AppNavigation.Screen renderedScreen;
    private String renderedNetworkId;
    private HomeScreen homeScreen;
    private AddNetworkScreen addNetworkScreen;
    private NetworkDetailScreen networkDetailScreen;

    private P2pVpnService.LocalBinder binder;
    private P2pVpnService.Snapshot latestSnapshot;
    private boolean bound;
    private String pendingDiagnosticReport;
    private String pendingEnableNetworkId;
    private String pendingJoinCode;
    private String pendingJoinHostname;
    private String pendingMutationNetworkId;
    private Boolean pendingMutationEnabled;
    private boolean mutationObservedBusy;
    private int mutationIdleObservations;
    private boolean networkCreationPending;
    private boolean networkCreationObservedBusy;
    private int networksAtCreationRequest;
    private int creationIdleObservations;
    private String selectionRequestNetworkId;
    private String localStatus;
    private OnBackInvokedCallback backInvokedCallback;

    private final ServiceConnection serviceConnection =
            new ServiceConnection() {
                @Override
                public void onServiceConnected(ComponentName name, IBinder service) {
                    binder = (P2pVpnService.LocalBinder) service;
                    bound = true;
                    binder.addListener(MainActivity.this);
                    renderCurrentScreen();
                }

                @Override
                public void onServiceDisconnected(ComponentName name) {
                    if (binder != null) {
                        binder.removeListener(MainActivity.this);
                    }
                    binder = null;
                    bound = false;
                    showLocalStatus("VPN service stopped");
                }
            };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_main);
        screenContainer = findViewById(R.id.screen_container);
        restoreState(savedInstanceState);
        if (Build.VERSION.SDK_INT >= 33) {
            backInvokedCallback = this::handleBackNavigation;
            getOnBackInvokedDispatcher()
                    .registerOnBackInvokedCallback(
                            OnBackInvokedDispatcher.PRIORITY_DEFAULT, backInvokedCallback);
        }
        inflateCurrentScreen();
        requestNotificationPermission();
    }

    private void restoreState(Bundle state) {
        if (state == null) {
            return;
        }
        navigation =
                AppNavigation.restore(
                        state.getString(STATE_NAVIGATION_SCREEN),
                        state.getString(STATE_NAVIGATION_NETWORK));
        pendingDiagnosticReport = state.getString(STATE_DIAGNOSTIC_REPORT);
        pendingEnableNetworkId = state.getString(STATE_PENDING_ENABLE_NETWORK);
        pendingMutationNetworkId = state.getString(STATE_PENDING_MUTATION_NETWORK);
        if (state.getBoolean(STATE_PENDING_MUTATION_PRESENT, false)) {
            pendingMutationEnabled = state.getBoolean(STATE_PENDING_MUTATION_ENABLED);
        }
        mutationObservedBusy = state.getBoolean(STATE_MUTATION_OBSERVED_BUSY, false);
        mutationIdleObservations = state.getInt(STATE_MUTATION_IDLE_OBSERVATIONS, 0);
        networkCreationPending = state.getBoolean(STATE_CREATION_PENDING, false);
        networkCreationObservedBusy = state.getBoolean(STATE_CREATION_OBSERVED_BUSY, false);
        networksAtCreationRequest = state.getInt(STATE_CREATION_NETWORK_COUNT, 0);
        creationIdleObservations = state.getInt(STATE_CREATION_IDLE_OBSERVATIONS, 0);
    }

    @Override
    protected void onStart() {
        super.onStart();
        bindService(
                new Intent(this, P2pVpnService.class),
                serviceConnection,
                Context.BIND_AUTO_CREATE);
    }

    @Override
    protected void onStop() {
        if (bound) {
            binder.removeListener(this);
            unbindService(serviceConnection);
            binder = null;
            bound = false;
        }
        super.onStop();
    }

    @Override
    protected void onDestroy() {
        if (Build.VERSION.SDK_INT >= 33 && backInvokedCallback != null) {
            getOnBackInvokedDispatcher().unregisterOnBackInvokedCallback(backInvokedCallback);
            backInvokedCallback = null;
        }
        super.onDestroy();
    }

    @Override
    protected void onSaveInstanceState(Bundle state) {
        super.onSaveInstanceState(state);
        state.putString(STATE_NAVIGATION_SCREEN, navigation.screen.name());
        putOptionalString(state, STATE_NAVIGATION_NETWORK, navigation.networkId);
        putOptionalString(state, STATE_DIAGNOSTIC_REPORT, pendingDiagnosticReport);
        putOptionalString(state, STATE_PENDING_ENABLE_NETWORK, pendingEnableNetworkId);
        if (pendingMutationNetworkId != null && pendingMutationEnabled != null) {
            state.putString(STATE_PENDING_MUTATION_NETWORK, pendingMutationNetworkId);
            state.putBoolean(STATE_PENDING_MUTATION_ENABLED, pendingMutationEnabled);
            state.putBoolean(STATE_PENDING_MUTATION_PRESENT, true);
        }
        state.putBoolean(STATE_MUTATION_OBSERVED_BUSY, mutationObservedBusy);
        state.putInt(STATE_MUTATION_IDLE_OBSERVATIONS, mutationIdleObservations);
        state.putBoolean(STATE_CREATION_PENDING, networkCreationPending);
        state.putBoolean(STATE_CREATION_OBSERVED_BUSY, networkCreationObservedBusy);
        state.putInt(STATE_CREATION_NETWORK_COUNT, networksAtCreationRequest);
        state.putInt(STATE_CREATION_IDLE_OBSERVATIONS, creationIdleObservations);
    }

    private static void putOptionalString(Bundle state, String key, String value) {
        if (value != null) {
            state.putString(key, value);
        }
    }

    @Override
    @SuppressLint("GestureBackNavigation")
    @SuppressWarnings("deprecation")
    public void onBackPressed() {
        handleBackNavigation();
    }

    private void handleBackNavigation() {
        if (navigation.screen == AppNavigation.Screen.HOME) {
            finishAfterTransition();
            return;
        }
        navigate(navigation.back());
    }

    @Override
    public void onSnapshot(P2pVpnService.Snapshot snapshot) {
        latestSnapshot = snapshot;
        if (navigation.screen == AppNavigation.Screen.JOIN
                && snapshot.profileJoinActive
                && !networkCreationPending) {
            networkCreationPending = true;
            networkCreationObservedBusy = true;
            networksAtCreationRequest = snapshot.networks.size();
            creationIdleObservations = 0;
        }
        reconcilePendingMutation(snapshot);
        reconcilePendingCreation(snapshot);

        AppNavigation reconciled = navigation.reconcile(networkIds(snapshot));
        boolean navigationChanged =
                reconciled.screen != navigation.screen
                        || !sameValue(reconciled.networkId, navigation.networkId);
        navigation = reconciled;
        if (navigationChanged || screenNeedsInflation()) {
            inflateCurrentScreen();
        }
        selectDetailNetworkIfNeeded(snapshot);
        renderCurrentScreen();
    }

    private void reconcilePendingMutation(P2pVpnService.Snapshot snapshot) {
        if (pendingMutationNetworkId == null || pendingMutationEnabled == null) {
            return;
        }
        if (snapshot.busy) {
            mutationObservedBusy = true;
            mutationIdleObservations = 0;
        }
        P2pVpnService.NetworkSnapshot network =
                findNetwork(snapshot, pendingMutationNetworkId);
        if (network != null && network.enabled == pendingMutationEnabled) {
            clearPendingMutation();
        } else if (mutationObservedBusy && !snapshot.busy) {
            clearPendingMutation();
        } else if (!snapshot.busy && ++mutationIdleObservations >= 2) {
            clearPendingMutation();
        }
    }

    private void clearPendingMutation() {
        pendingMutationNetworkId = null;
        pendingMutationEnabled = null;
        mutationObservedBusy = false;
        mutationIdleObservations = 0;
    }

    private void reconcilePendingCreation(P2pVpnService.Snapshot snapshot) {
        if (!networkCreationPending) {
            return;
        }
        if (snapshot.networks.size() > networksAtCreationRequest
                && snapshot.selectedNetworkId != null) {
            networkCreationPending = false;
            networkCreationObservedBusy = false;
            creationIdleObservations = 0;
            navigation = AppNavigation.detail(snapshot.selectedNetworkId);
            localStatus = null;
            inflateCurrentScreen();
            return;
        }
        if (snapshot.busy) {
            networkCreationObservedBusy = true;
            creationIdleObservations = 0;
            return;
        }
        if (!networkCreationObservedBusy && ++creationIdleObservations < 2) {
            return;
        }
        networkCreationPending = false;
        networkCreationObservedBusy = false;
        creationIdleObservations = 0;
    }

    private void navigate(AppNavigation destination) {
        navigation = destination;
        localStatus = null;
        inflateCurrentScreen();
        if (latestSnapshot != null) {
            selectDetailNetworkIfNeeded(latestSnapshot);
        }
        renderCurrentScreen();
    }

    private boolean screenNeedsInflation() {
        return renderedScreen != navigation.screen
                || (navigation.screen == AppNavigation.Screen.DETAIL
                        && !sameValue(renderedNetworkId, navigation.networkId));
    }

    private void inflateCurrentScreen() {
        int layout;
        switch (navigation.screen) {
            case HOME:
                layout = R.layout.screen_home;
                break;
            case ADD:
            case CREATE:
            case JOIN:
                layout = R.layout.screen_add_network;
                break;
            case DETAIL:
                layout = R.layout.screen_network_detail;
                break;
            default:
                throw new IllegalStateException("Unsupported application screen");
        }
        screenContainer.removeAllViews();
        View root = LayoutInflater.from(this).inflate(layout, screenContainer, false);
        screenContainer.addView(root);
        renderedScreen = navigation.screen;
        renderedNetworkId = navigation.networkId;
        homeScreen = null;
        addNetworkScreen = null;
        networkDetailScreen = null;
        bindScreen(root);
        renderCurrentScreen();
    }

    private void bindScreen(View root) {
        switch (navigation.screen) {
            case HOME:
                homeScreen = new HomeScreen(this, root, homeListener());
                break;
            case ADD:
            case CREATE:
            case JOIN:
                addNetworkScreen =
                        new AddNetworkScreen(this, root, navigation.screen, addListener());
                break;
            case DETAIL:
                networkDetailScreen =
                        new NetworkDetailScreen(
                                this, root, navigation.networkId, detailListener());
                break;
            default:
                break;
        }
    }

    private HomeScreen.Listener homeListener() {
        return new HomeScreen.Listener() {
            @Override
            public void addNetwork() {
                navigate(navigation.openAdd());
            }

            @Override
            public void exportDiagnostics() {
                MainActivity.this.exportDiagnostics();
            }

            @Override
            public void resetUnreadableProfile() {
                confirmResetUnreadableProfile();
            }

            @Override
            public void openNetwork(String networkId) {
                navigate(AppNavigation.detail(networkId));
            }

            @Override
            public void setNetworkEnabled(String networkId, boolean enabled) {
                requestNetworkEnabled(networkId, enabled);
            }

        };
    }

    private AddNetworkScreen.Listener addListener() {
        return new AddNetworkScreen.Listener() {
            @Override
            public void back() {
                navigate(navigation.back());
            }

            @Override
            public void showCreate() {
                navigate(navigation.openCreate());
            }

            @Override
            public void showJoin() {
                navigate(navigation.openJoin());
            }

            @Override
            public void createNetwork(String networkName) {
                MainActivity.this.createNetwork(networkName);
            }

            @Override
            public void joinNetwork(String pairingCode, String hostname) {
                MainActivity.this.joinNetwork(pairingCode, hostname);
            }

            @Override
            public void cancelJoin() {
                MainActivity.this.cancelJoin();
            }
        };
    }

    private NetworkDetailScreen.Listener detailListener() {
        return new NetworkDetailScreen.Listener() {
            @Override
            public void back() {
                navigate(navigation.back());
            }

            @Override
            public void setNetworkEnabled(String networkId, boolean enabled) {
                requestNetworkEnabled(networkId, enabled);
            }

            @Override
            public void renameNetwork(String networkId, String hostname) {
                if (binder != null) {
                    binder.renameNetwork(networkId, hostname);
                }
            }

            @Override
            public void openPairing() {
                if (binder != null) {
                    binder.openPairing();
                }
            }

            @Override
            public void approvePairing(String hostname) {
                if (binder != null) {
                    binder.approvePairing(hostname);
                }
            }

            @Override
            public void rejectPairing() {
                if (binder != null) {
                    binder.rejectPairing();
                }
            }

            @Override
            public void copyPairingCode(CharSequence code) {
                MainActivity.this.copyPairingCode(code);
            }

            @Override
            public void revokeMember(
                    P2pVpnService.NetworkSnapshot network, PeerSnapshot.Peer peer) {
                confirmRevokeMember(network, peer);
            }

            @Override
            public void resignMembership(P2pVpnService.NetworkSnapshot network) {
                confirmResignMembership(network);
            }

            @Override
            public void removeNetwork(P2pVpnService.NetworkSnapshot network) {
                confirmRemoveNetwork(network);
            }
        };
    }

    private void renderCurrentScreen() {
        String statusText = latestSnapshot == null ? "" : statusText(latestSnapshot);
        if (homeScreen != null) {
            homeScreen.render(
                    latestSnapshot,
                    bound,
                    pendingEnableNetworkId,
                    pendingMutationNetworkId,
                    pendingMutationEnabled,
                    statusText);
        } else if (addNetworkScreen != null) {
            addNetworkScreen.render(
                    latestSnapshot, bound, networkCreationPending, statusText);
        } else if (networkDetailScreen != null) {
            networkDetailScreen.render(
                    latestSnapshot,
                    bound,
                    pendingEnableNetworkId,
                    pendingMutationNetworkId,
                    pendingMutationEnabled,
                    statusText);
        }
    }

    private void createNetwork(String networkName) {
        if (binder == null) {
            showLocalStatus("VPN service is not ready");
            return;
        }
        networkCreationPending = true;
        networkCreationObservedBusy = false;
        creationIdleObservations = 0;
        networksAtCreationRequest =
                latestSnapshot == null ? 0 : latestSnapshot.networks.size();
        localStatus = null;
        binder.createProfile(networkName);
        renderCurrentScreen();
    }

    private void joinNetwork(String pairingCode, String hostname) {
        if (pendingEnableNetworkId != null || pendingJoinCode != null) {
            return;
        }
        localStatus = null;
        if (!LocalNetworkPermission.isGranted(this)) {
            pendingJoinCode = pairingCode;
            pendingJoinHostname = hostname;
            requestPermissions(
                    new String[] {LocalNetworkPermission.NAME},
                    LOCAL_NETWORK_PERMISSION_REQUEST);
            renderCurrentScreen();
            return;
        }
        startProfileJoin(pairingCode, hostname);
    }

    private void startProfileJoin(String pairingCode, String hostname) {
        networkCreationPending = true;
        networkCreationObservedBusy = false;
        creationIdleObservations = 0;
        networksAtCreationRequest =
                latestSnapshot == null ? 0 : latestSnapshot.networks.size();
        Intent intent = new Intent(this, P2pVpnService.class);
        intent.setAction(P2pVpnService.ACTION_JOIN_PROFILE);
        intent.putExtra(P2pVpnService.EXTRA_PAIRING_CODE, pairingCode);
        intent.putExtra(P2pVpnService.EXTRA_PAIRING_HOSTNAME, hostname);
        startForegroundService(intent);
        renderCurrentScreen();
    }

    private void cancelJoin() {
        Intent intent = new Intent(this, P2pVpnService.class);
        intent.setAction(P2pVpnService.ACTION_CANCEL_PROFILE_JOIN);
        startService(intent);
    }

    private void selectDetailNetworkIfNeeded(P2pVpnService.Snapshot snapshot) {
        if (navigation.screen != AppNavigation.Screen.DETAIL || binder == null) {
            return;
        }
        P2pVpnService.NetworkSnapshot network = findNetwork(snapshot, navigation.networkId);
        if (network == null || network.selected) {
            selectionRequestNetworkId = null;
            return;
        }
        if (snapshot.busy
                || snapshot.pairingActive
                || network.id.equals(selectionRequestNetworkId)) {
            return;
        }
        selectionRequestNetworkId = network.id;
        binder.selectNetwork(network.id);
    }

    private void confirmResetUnreadableProfile() {
        new AlertDialog.Builder(this)
                .setTitle(R.string.reset_profile_title)
                .setMessage(R.string.reset_profile_warning)
                .setNegativeButton(android.R.string.cancel, null)
                .setPositiveButton(
                        R.string.reset_profile,
                        (dialog, which) -> {
                            if (binder != null) {
                                binder.resetUnreadableProfile();
                            }
                        })
                .show();
    }

    private void confirmRemoveNetwork(P2pVpnService.NetworkSnapshot network) {
        new AlertDialog.Builder(this)
                .setTitle(getString(R.string.remove_network_title, network.name))
                .setMessage(R.string.remove_network_warning)
                .setNegativeButton(android.R.string.cancel, null)
                .setPositiveButton(
                        R.string.remove_network,
                        (dialog, which) -> {
                            if (binder != null) {
                                binder.removeNetwork(network.id);
                            }
                        })
                .show();
    }

    private void confirmRevokeMember(
            P2pVpnService.NetworkSnapshot network, PeerSnapshot.Peer peer) {
        String name = peer.hostnames.isEmpty() ? peer.peerId : peer.hostnames.get(0);
        new AlertDialog.Builder(this)
                .setTitle(getString(R.string.revoke_member_title, name))
                .setMessage(R.string.revoke_member_warning)
                .setNegativeButton(android.R.string.cancel, null)
                .setPositiveButton(
                        R.string.revoke_member,
                        (dialog, which) -> {
                            if (binder != null) {
                                binder.revokeMember(network.id, peer.peerId);
                            }
                        })
                .show();
    }

    private void confirmResignMembership(P2pVpnService.NetworkSnapshot network) {
        new AlertDialog.Builder(this)
                .setTitle(getString(R.string.resign_membership_title, network.name))
                .setMessage(R.string.resign_membership_warning)
                .setNegativeButton(android.R.string.cancel, null)
                .setPositiveButton(
                        R.string.resign_membership,
                        (dialog, which) -> {
                            if (binder != null) {
                                binder.resignMembership(network.id);
                            }
                        })
                .show();
    }

    private void copyPairingCode(CharSequence code) {
        if (code == null || code.length() == 0) {
            return;
        }
        ClipboardManager clipboard = getSystemService(ClipboardManager.class);
        ClipData clip = ClipData.newPlainText("p2p-vpn code", code);
        if (Build.VERSION.SDK_INT >= 33) {
            PersistableBundle extras = new PersistableBundle();
            extras.putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true);
            clip.getDescription().setExtras(extras);
        }
        clipboard.setPrimaryClip(clip);
        Toast.makeText(this, R.string.pairing_code_copied, Toast.LENGTH_SHORT).show();
    }

    private String statusText(P2pVpnService.Snapshot snapshot) {
        String base =
                snapshot.vpnModeDetail
                        + "\n"
                        + snapshot.connectionDetail
                        + "\n"
                        + snapshot.pairingDetail;
        return localStatus == null || localStatus.isEmpty()
                ? base
                : localStatus + "\n" + base;
    }

    private static P2pVpnService.NetworkSnapshot findNetwork(
            P2pVpnService.Snapshot snapshot, String networkId) {
        if (snapshot == null || networkId == null) {
            return null;
        }
        for (P2pVpnService.NetworkSnapshot network : snapshot.networks) {
            if (network.id.equals(networkId)) {
                return network;
            }
        }
        return null;
    }

    private static List<String> networkIds(P2pVpnService.Snapshot snapshot) {
        List<String> result = new ArrayList<>(snapshot.networks.size());
        for (P2pVpnService.NetworkSnapshot network : snapshot.networks) {
            result.add(network.id);
        }
        return result;
    }

    private static boolean sameValue(String first, String second) {
        return first == null ? second == null : first.equals(second);
    }

    @Override
    @SuppressWarnings("deprecation")
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == VPN_PERMISSION_REQUEST) {
            if (resultCode == RESULT_OK) {
                completePendingVpnRequest();
            } else {
                pendingEnableNetworkId = null;
                showLocalStatus("VPN permission was not granted");
            }
        } else if (requestCode == DIAGNOSTIC_EXPORT_REQUEST) {
            if (resultCode == RESULT_OK && data != null && data.getData() != null) {
                writeDiagnosticReport(data.getData());
            } else {
                pendingDiagnosticReport = null;
            }
        }
    }

    @Override
    public void onRequestPermissionsResult(
            int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode != LOCAL_NETWORK_PERMISSION_REQUEST) {
            return;
        }
        if (LocalNetworkPermission.isGranted(this)) {
            if (pendingJoinCode != null && pendingJoinHostname != null) {
                String code = pendingJoinCode;
                String hostname = pendingJoinHostname;
                pendingJoinCode = null;
                pendingJoinHostname = null;
                startProfileJoin(code, hostname);
            } else {
                prepareVpnRequest();
            }
        } else {
            pendingEnableNetworkId = null;
            pendingJoinCode = null;
            pendingJoinHostname = null;
            showLocalStatus("Local network permission was not granted");
        }
    }

    private void requestNetworkEnabled(String networkId, boolean enabled) {
        if (pendingEnableNetworkId != null
                || pendingMutationNetworkId != null
                || pendingJoinCode != null) {
            return;
        }
        localStatus = null;
        if (!enabled) {
            startNetworkActivationService(networkId, false);
            return;
        }
        pendingEnableNetworkId = networkId;
        if (!LocalNetworkPermission.isGranted(this)) {
            requestPermissions(
                    new String[] {LocalNetworkPermission.NAME},
                    LOCAL_NETWORK_PERMISSION_REQUEST);
            renderCurrentScreen();
            return;
        }
        prepareVpnRequest();
    }

    @SuppressWarnings("deprecation")
    private void prepareVpnRequest() {
        Intent permission = VpnService.prepare(this);
        if (permission == null) {
            completePendingVpnRequest();
        } else {
            startActivityForResult(permission, VPN_PERMISSION_REQUEST);
        }
    }

    private void completePendingVpnRequest() {
        String networkId = pendingEnableNetworkId;
        pendingEnableNetworkId = null;
        if (networkId == null) {
            showLocalStatus("No network is waiting for VPN permission");
            return;
        }
        startNetworkActivationService(networkId, true);
    }

    private void startNetworkActivationService(String networkId, boolean enabled) {
        pendingMutationNetworkId = networkId;
        pendingMutationEnabled = enabled;
        mutationObservedBusy = false;
        mutationIdleObservations = 0;
        Intent intent = new Intent(this, P2pVpnService.class);
        intent.setAction(P2pVpnService.ACTION_SET_NETWORK_ENABLED);
        intent.putExtra(P2pVpnService.EXTRA_NETWORK_ID, networkId);
        intent.putExtra(P2pVpnService.EXTRA_NETWORK_ENABLED, enabled);
        startForegroundService(intent);
        renderCurrentScreen();
    }

    private void exportDiagnostics() {
        if (binder == null) {
            Toast.makeText(this, R.string.diagnostics_unavailable, Toast.LENGTH_SHORT).show();
            return;
        }
        try {
            pendingDiagnosticReport = binder.createDiagnosticReport();
            Intent destination = new Intent(Intent.ACTION_CREATE_DOCUMENT);
            destination.addCategory(Intent.CATEGORY_OPENABLE);
            destination.setType("application/json");
            destination.putExtra(Intent.EXTRA_TITLE, "p2p-vpn-diagnostics.json");
            startActivityForResult(destination, DIAGNOSTIC_EXPORT_REQUEST);
        } catch (RuntimeException error) {
            pendingDiagnosticReport = null;
            Toast.makeText(this, R.string.diagnostics_unavailable, Toast.LENGTH_SHORT).show();
        }
    }

    private void writeDiagnosticReport(Uri destination) {
        String report = pendingDiagnosticReport;
        pendingDiagnosticReport = null;
        if (report == null) {
            Toast.makeText(this, R.string.diagnostics_unavailable, Toast.LENGTH_SHORT).show();
            return;
        }
        Context application = getApplicationContext();
        new Thread(
                        () -> {
                            boolean written = false;
                            try (OutputStream output =
                                    application
                                            .getContentResolver()
                                            .openOutputStream(destination, "wt")) {
                                if (output != null) {
                                    output.write(report.getBytes(StandardCharsets.UTF_8));
                                    written = true;
                                }
                            } catch (IOException | RuntimeException ignored) {
                                // The selected document provider reports the failure to the user.
                            }
                            int message =
                                    written
                                            ? R.string.diagnostics_exported
                                            : R.string.diagnostics_export_failed;
                            runOnUiThread(
                                    () ->
                                            Toast.makeText(
                                                            application,
                                                            message,
                                                            Toast.LENGTH_SHORT)
                                                    .show());
                        },
                        "p2p-vpn-diagnostic-export")
                .start();
    }

    private void requestNotificationPermission() {
        if (Build.VERSION.SDK_INT >= 33
                && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                        != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(
                    new String[] {Manifest.permission.POST_NOTIFICATIONS},
                    NOTIFICATION_PERMISSION_REQUEST);
        }
    }

    private void showLocalStatus(String message) {
        localStatus = message;
        renderCurrentScreen();
    }
}
