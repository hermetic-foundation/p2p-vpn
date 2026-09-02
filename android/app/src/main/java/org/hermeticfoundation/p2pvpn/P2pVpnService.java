package org.hermeticfoundation.p2pvpn;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.net.VpnManager;
import android.net.VpnProfileState;
import android.net.VpnService;
import android.net.wifi.WifiManager;
import android.os.Binder;
import android.os.Build;
import android.os.Debug;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.ParcelFileDescriptor;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;
import java.io.File;
import java.io.IOException;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.StandardCopyOption;
import java.time.Instant;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Optional;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.ScheduledThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

public final class P2pVpnService extends VpnService {
    private static final String LOG_TAG = "p2p-vpn";
    static final String ACTION_CONNECT = "org.hermeticfoundation.p2pvpn.CONNECT";
    static final String ACTION_DISCONNECT = "org.hermeticfoundation.p2pvpn.DISCONNECT";
    static final String ACTION_SET_NETWORK_ENABLED =
            "org.hermeticfoundation.p2pvpn.SET_NETWORK_ENABLED";
    static final String ACTION_JOIN_PROFILE = "org.hermeticfoundation.p2pvpn.JOIN_PROFILE";
    static final String ACTION_CANCEL_PROFILE_JOIN =
            "org.hermeticfoundation.p2pvpn.CANCEL_PROFILE_JOIN";
    static final String ACTION_DEBUG_COMMAND = "org.hermeticfoundation.p2pvpn.debug.COMMAND";
    static final String EXTRA_NETWORK_ID = "network_id";
    static final String EXTRA_NETWORK_ENABLED = "network_enabled";
    static final String EXTRA_PAIRING_CODE = "pairing_code";
    static final String EXTRA_PAIRING_HOSTNAME = "pairing_hostname";
    static final String EXTRA_DEBUG_COMMAND = "command";
    static final String EXTRA_DEBUG_VALUE = "value";
    static final int DEBUG_PACKET_QUIC_ENDPOINT_MAX_LENGTH = 512;
    static final int DEBUG_RELAY_RESERVATION_MAX_LENGTH = 1_024;
    static final int DEBUG_ADDITIONAL_ROUTE_MAX_LENGTH = 64;

    private static final String NOTIFICATION_CHANNEL = "p2p-vpn-connection";
    private static final int NOTIFICATION_ID = 1;
    private static final long PAIRING_TIMEOUT_SECONDS = 600;
    private static final long PAIRING_POLL_MILLIS = 1_000;
    private static final long STATUS_POLL_MILLIS = 2_000;
    private static final long BLOCKED_MODE_POLL_MILLIS = 30_000;
    private static final long NETWORK_RECONNECT_DELAY_MILLIS = 1_500;
    private static final long MAX_RECONNECT_DELAY_MILLIS = 30_000;
    private static final long UNDERLAY_RECOVERY_DELAY_MILLIS = 500;
    private static volatile P2pVpnService debugInstance;

    private final LocalBinder localBinder = new LocalBinder();
    private final Handler mainHandler = new Handler(Looper.getMainLooper());
    private final Set<Listener> listeners =
            Collections.newSetFromMap(new ConcurrentHashMap<Listener, Boolean>());
    private final UnderlayTracker underlayTracker = new UnderlayTracker();
    private final UnderlayRecoveryPolicy underlayRecoveryPolicy = new UnderlayRecoveryPolicy();
    private final DiagnosticEventBuffer diagnosticEvents = new DiagnosticEventBuffer();

    private ScheduledThreadPoolExecutor worker;
    private ExecutorService profileJoinWorker;
    private ProfileStore profileStore;
    private File runtimeDirectory;
    private Map<String, RuntimeFiles> runtimeFiles = Collections.emptyMap();
    private boolean runtimeStorageReady;
    private ConnectivityManager connectivityManager;
    private ConnectivityManager.NetworkCallback networkCallback;
    private WifiManager.MulticastLock multicastLock;
    private WifiManager.MulticastLock pairingMulticastLock;
    private ScheduledFuture<?> reconnectFuture;
    private ScheduledFuture<?> underlayRecoveryFuture;
    private ScheduledFuture<?> statusFuture;
    private ScheduledFuture<?> pairingFuture;

    private AndroidProfile profile;
    private ProfileCollection profileCollection;
    private Map<String, AndroidProfile> inspectedProfiles = Collections.emptyMap();
    private List<String> activeNetworkIds = Collections.emptyList();
    private Map<String, NetworkRuntimeStatus> networkRuntimeStatuses = Collections.emptyMap();
    private String selectedNetworkId;
    private boolean profilePresent;
    private boolean profileUnreadable;
    private ActivePairing activePairing;
    private volatile ProfileJoinOperation profileJoinOperation;
    private boolean desiredConnected;
    private boolean connected;
    private boolean operationInProgress;
    private String connectionDetail = "Disconnected";
    private String peerDetail = "Overlay peers: unavailable";
    private String pairingDetail = "No pairing operation";
    private String reconnectDetail = "Recovering connection";
    private RuntimeSummary latestRuntimeSummary = RuntimeSummary.empty();
    private RuntimeDiagnostics latestRuntimeDiagnostics = RuntimeDiagnostics.empty();
    private final RuntimeSummaryAccumulator runtimeSummaryAccumulator =
            new RuntimeSummaryAccumulator();
    private int reconnectAttempts;
    private long runtimeGeneration;
    private long runtimeNetworkChangeRequests;
    private long runtimeNetworkChangeFailures;
    private long serviceStartedElapsedRealtime;
    private volatile boolean legacySystemStartObserved;
    private volatile boolean vpnModeObservationGap;
    private volatile VpnMode vpnMode = VpnMode.manual();
    private volatile Snapshot snapshot = Snapshot.initial();

    @Override
    public void onCreate() {
        super.onCreate();
        serviceStartedElapsedRealtime = SystemClock.elapsedRealtime();
        recordDiagnosticEvent("service_created");
        debugInstance = this;
        worker = new ScheduledThreadPoolExecutor(1);
        worker.setRemoveOnCancelPolicy(true);
        profileJoinWorker =
                Executors.newSingleThreadExecutor(
                        runnable -> {
                            Thread thread = new Thread(runnable, "p2p-vpn-profile-join");
                            thread.setDaemon(true);
                            return thread;
                        });
        profileStore = new ProfileStore(this);
        prepareRuntimeDirectory();
        createNotificationChannel();
        registerNetworkCallback();
        worker.execute(this::loadProfileMetadata);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? null : intent.getAction();
        VpnMode managerEventMode = vpnManagerEventMode(intent);
        boolean systemStart = isSystemVpnStart(action);
        refreshVpnMode(systemStart);
        if (managerEventMode != null) {
            if (managerEventMode.alwaysOn || desiredConnected) {
                startForeground(
                        NOTIFICATION_ID,
                        notification(getString(R.string.notification_connecting)));
            }
            worker.execute(() -> vpnManagerModeChanged(managerEventMode));
        } else if (ACTION_CONNECT.equals(action)) {
            startForeground(
                    NOTIFICATION_ID,
                    notification(getString(R.string.notification_connecting)));
            worker.execute(() -> connectRequested(false));
        } else if (ACTION_DISCONNECT.equals(action)) {
            worker.execute(() -> disconnectRequested(false));
        } else if (ACTION_SET_NETWORK_ENABLED.equals(action) && intent != null) {
            startForeground(
                    NOTIFICATION_ID,
                    notification(getString(R.string.notification_connecting)));
            String networkId = intent.getStringExtra(EXTRA_NETWORK_ID);
            boolean enabled = intent.getBooleanExtra(EXTRA_NETWORK_ENABLED, false);
            worker.execute(() -> setNetworkEnabled(networkId, enabled));
        } else if (ACTION_JOIN_PROFILE.equals(action) && intent != null) {
            startForeground(
                    NOTIFICATION_ID,
                    notification(getString(R.string.notification_pairing)));
            String code = intent.getStringExtra(EXTRA_PAIRING_CODE);
            String hostname = intent.getStringExtra(EXTRA_PAIRING_HOSTNAME);
            worker.execute(() -> joinProfileByCode(code, hostname));
        } else if (ACTION_CANCEL_PROFILE_JOIN.equals(action)) {
            worker.execute(this::cancelProfileJoin);
        } else if (ACTION_DEBUG_COMMAND.equals(action) && isDebuggable()) {
            String command = intent.getStringExtra(EXTRA_DEBUG_COMMAND);
            String value = intent.getStringExtra(EXTRA_DEBUG_VALUE);
            if (command != null) {
                worker.execute(() -> executeDebugCommand(command, value));
            }
        } else if (systemStart) {
            startForeground(
                    NOTIFICATION_ID,
                    notification(getString(R.string.notification_connecting)));
            worker.execute(this::restorePersistedActivation);
        }
        boolean managerAlwaysOn = managerEventMode != null && managerEventMode.alwaysOn;
        return shouldRestartAfterProcessDeath(
                        action,
                        systemStart,
                        desiredConnected,
                        vpnMode.alwaysOn || managerAlwaysOn)
                ? START_STICKY
                : START_NOT_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        if (intent != null && SERVICE_INTERFACE.equals(intent.getAction())) {
            return super.onBind(intent);
        }
        return localBinder;
    }

    @Override
    public void onRevoke() {
        worker.execute(() -> disconnectRequested(true));
    }

    @Override
    public void onDestroy() {
        if (connectivityManager != null && networkCallback != null) {
            try {
                connectivityManager.unregisterNetworkCallback(networkCallback);
            } catch (IllegalArgumentException ignored) {
                // The callback may already have been unregistered during process teardown.
            }
        }
        cancel(reconnectFuture);
        cancel(underlayRecoveryFuture);
        cancel(statusFuture);
        cancel(pairingFuture);
        cancelProfileJoinBestEffort();
        if (profileJoinWorker != null) {
            profileJoinWorker.shutdownNow();
        }
        releasePairingMulticastLock();
        if (worker != null && !worker.isShutdown()) {
            try {
                worker.submit(this::stopNativeRuntime).get(6, TimeUnit.SECONDS);
            } catch (Exception ignored) {
                releaseMulticastLock();
            }
            worker.shutdownNow();
        }
        if (debugInstance == this) {
            debugInstance = null;
        }
        super.onDestroy();
    }

    private void connectRequested(boolean systemStart) {
        refreshVpnMode(systemStart);
        if (!desiredConnected) {
            recordDiagnosticEvent(
                    systemStart ? "always_on_start_requested" : "connection_requested");
        }
        desiredConnected = true;
        if (connected || operationInProgress) {
            return;
        }
        startConnection(systemStart ? "Starting always-on VPN" : "Connecting");
    }

    private void restorePersistedActivation() {
        refreshVpnMode(true);
        if (enabledNetworkCount(profileCollection) > 0) {
            connectRequested(true);
            return;
        }
        if (vpnMode.alwaysOn) {
            desiredConnected = true;
            connectionDetail = "No networks enabled";
            peerDetail = "Overlay peers: unavailable";
            updateForegroundNotification();
            publishSnapshot();
            return;
        }
        disconnectRequested(true);
    }

    private void vpnManagerModeChanged(VpnMode mode) {
        recordDiagnosticEvent("vpn_manager_mode_event");
        vpnModeObservationGap = false;
        applyVpnMode(mode);
        if (mode.alwaysOn) {
            desiredConnected = true;
        }
        if (mode.lockdown) {
            cancel(reconnectFuture);
            reconnectFuture = null;
            cancel(underlayRecoveryFuture);
            underlayRecoveryFuture = null;
            if (connected) {
                stopNativeRuntime();
            }
            reportLockdownBlocked();
            if (desiredConnected) {
                scheduleBlockedModePoll();
            }
            return;
        }
        cancel(statusFuture);
        statusFuture = null;
        if (desiredConnected && !connected && !operationInProgress) {
            startConnection("Restoring after VPN mode changed");
        } else {
            publishSnapshot();
        }
    }

    private void disconnectRequested(boolean force) {
        refreshVpnMode(false);
        if (!force && !vpnMode.permitsDisconnect()) {
            desiredConnected = true;
            connectionDetail = "Always-on VPN is managed by Android";
            recordDiagnosticEvent("always_on_disconnect_ignored");
            updateForegroundNotification();
            publishSnapshot();
            if (!connected && !operationInProgress) {
                startConnection("Restoring always-on VPN");
            }
            return;
        }
        desiredConnected = false;
        recordDiagnosticEvent("disconnect_requested");
        cancel(reconnectFuture);
        reconnectFuture = null;
        cancel(underlayRecoveryFuture);
        underlayRecoveryFuture = null;
        cancelActivePairingBestEffort();
        clearActivePairing();
        cancel(pairingFuture);
        pairingFuture = null;
        stopNativeRuntime();
        connectionDetail = "Disconnected";
        peerDetail = "Overlay peers: unavailable";
        pairingDetail = "No pairing operation";
        publishSnapshot();
        stopManualService();
    }

    private void startConnection(String initialDetail) {
        if (!desiredConnected || operationInProgress) {
            return;
        }
        refreshVpnMode(false);
        if (!vpnMode.permitsOverlayConnection()) {
            reportLockdownBlocked();
            scheduleBlockedModePoll();
            return;
        }
        if (!profilePresent) {
            connectionDetail = getString(R.string.always_on_profile_required);
            recordDiagnosticEvent("connection_waiting_for_profile");
            updateForegroundNotification();
            publishSnapshot();
            return;
        }
        if (profileUnreadable) {
            connectionDetail = getString(R.string.always_on_profile_unreadable);
            recordDiagnosticEvent("connection_waiting_for_readable_profile");
            updateForegroundNotification();
            publishSnapshot();
            return;
        }
        if (!LocalNetworkPermission.isGranted(this)) {
            stopForMissingLocalNetworkPermission();
            return;
        }
        operationInProgress = true;
        connectionDetail = initialDetail;
        peerDetail = "Overlay peers: discovering";
        publishSnapshot();
        boolean recoveryRequired = false;
        try {
            if (!runtimeStorageReady) {
                throw new P2pVpnException("Private runtime storage is unavailable");
            }
            loadProfile();
            if (!hasEnabledNetwork(profileCollection)) {
                connectionDetail = "Enable at least one network to connect";
                peerDetail = "Overlay peers: unavailable";
                updateForegroundNotification();
                return;
            }
            AndroidRuntimePlan runtimePlan =
                    AndroidRuntimePlan.create(
                            profileCollection, inspectedProfiles, runtimeStatePaths());
            NativeResponse.objectValue(
                    NativeBridge.nativeValidateStartNetworks(runtimePlan.requestJson));
            acquireMulticastLock();
            Builder builder = new Builder();
            builder.setSession(runtimePlan.sessionName);
            builder.setMtu(runtimePlan.mtu);
            builder.setBlocking(true);
            try {
                // Every socket created by this process is VPN underlay traffic.
                builder.addDisallowedApplication(getPackageName());
            } catch (PackageManager.NameNotFoundException error) {
                throw new P2pVpnException("Failed to isolate VPN transport sockets", error);
            }
            for (AndroidProfile.Cidr address : runtimePlan.addresses) {
                builder.addAddress(address.inetAddress, address.prefixLength);
            }
            for (AndroidProfile.Cidr route : runtimePlan.routes) {
                builder.addRoute(route.inetAddress, route.prefixLength);
            }
            Intent configureIntent = new Intent(this, MainActivity.class);
            PendingIntent pendingIntent =
                    PendingIntent.getActivity(
                            this,
                            0,
                            configureIntent,
                            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
            builder.setConfigureIntent(pendingIntent);

            ParcelFileDescriptor descriptor = builder.establish();
            if (descriptor == null) {
                throw new P2pVpnException("Android did not establish the VPN interface");
            }
            int tunFd;
            try {
                tunFd = descriptor.detachFd();
            } finally {
                try {
                    descriptor.close();
                } catch (IOException ignored) {
                    // detachFd transferred ownership of the descriptor to the native runtime.
                }
            }
            JSONObject initialStatus =
                    NativeResponse.objectValue(
                            NativeBridge.nativeStartNetworks(runtimePlan.requestJson, tunFd));
            RuntimeStatusSnapshot started =
                    RuntimeStatusSnapshot.from(initialStatus, runtimePlan.networkIds);
            activeNetworkIds = runtimePlan.networkIds;
            networkRuntimeStatuses = started.networks;
            latestRuntimeSummary =
                    runtimeSummaryAccumulator.observe(
                            RuntimeSummary.fromLines(started.metrics));
            latestRuntimeDiagnostics = RuntimeDiagnostics.fromLines(started.metrics);
            peerDetail = latestRuntimeSummary.describe();
            if (runtimeGeneration < Long.MAX_VALUE) {
                runtimeGeneration++;
            }
            connected = true;
            recordDiagnosticEvent("runtime_started");
            reconnectAttempts = 0;
            cancel(reconnectFuture);
            reconnectFuture = null;
            connectionDetail = started.describeConnection();
            scheduleStatusPoll();
            if (activePairing != null) {
                schedulePairingResume(0);
            }
            updateForegroundNotification();
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            String failure = failureMessage(error);
            recordDiagnosticEvent("runtime_start_failed");
            // nativeStart may have accepted the detached descriptor before reporting failure.
            stopNativeRuntime();
            connectionDetail = failure;
            recoveryRequired = desiredConnected;
            updateForegroundNotification();
        } finally {
            operationInProgress = false;
            publishSnapshot();
            if (recoveryRequired) {
                scheduleReconnect("Retrying after startup failure", false);
            }
        }
    }

    private void stopNativeRuntime() {
        boolean wasConnected = connected;
        underlayRecoveryPolicy.reset();
        cancel(statusFuture);
        statusFuture = null;
        try {
            NativeResponse.objectValue(NativeBridge.nativeStop());
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            connectionDetail = failureMessage(error);
        }
        connected = false;
        activeNetworkIds = Collections.emptyList();
        networkRuntimeStatuses = Collections.emptyMap();
        latestRuntimeDiagnostics = RuntimeDiagnostics.empty();
        if (wasConnected) {
            recordDiagnosticEvent("runtime_stopped");
        }
        peerDetail = "Overlay peers: unavailable";
        latestRuntimeSummary = runtimeSummaryAccumulator.finishRuntime();
        releaseMulticastLock();
    }

    private void reconnectAfterNetworkChange() {
        reconnectFuture = null;
        if (!desiredConnected) {
            return;
        }
        if (operationInProgress) {
            reconnectFuture =
                    worker.schedule(
                            this::reconnectAfterNetworkChange,
                            NETWORK_RECONNECT_DELAY_MILLIS,
                            TimeUnit.MILLISECONDS);
            return;
        }
        operationInProgress = true;
        connectionDetail = reconnectDetail;
        publishSnapshot();
        stopNativeRuntime();
        operationInProgress = false;
        startConnection(reconnectDetail);
    }

    private void scheduleReconnect(String detail, boolean resetBackoff) {
        if (!desiredConnected) {
            return;
        }
        reconnectDetail = detail;
        if (resetBackoff) {
            reconnectAttempts = 0;
        }
        int exponent = Math.min(reconnectAttempts, 4);
        long delay =
                Math.min(
                        NETWORK_RECONNECT_DELAY_MILLIS * (1L << exponent),
                        MAX_RECONNECT_DELAY_MILLIS);
        reconnectAttempts++;
        cancel(reconnectFuture);
        reconnectFuture =
                worker.schedule(this::reconnectAfterNetworkChange, delay, TimeUnit.MILLISECONDS);
    }

    private void handleUnderlayChange(UnderlayTracker.Change change) {
        switch (change) {
            case INITIAL:
                recordDiagnosticEvent("underlay_baseline_selected");
                break;
            case CHANGED:
                recordDiagnosticEvent("underlay_selection_changed");
                break;
            case LOST:
                recordDiagnosticEvent("underlay_lost");
                break;
            case RECOVERED:
                recordDiagnosticEvent("underlay_recovered");
                break;
            case AVAILABLE_CHANGED:
            case UNCHANGED:
                break;
        }
        if (change.requiresRuntimeRecovery() && desiredConnected) {
            underlayRecoveryPolicy.reset();
            cancel(underlayRecoveryFuture);
            underlayRecoveryFuture =
                    worker.schedule(
                            this::notifyNativeNetworkChanged,
                            UNDERLAY_RECOVERY_DELAY_MILLIS,
                            TimeUnit.MILLISECONDS);
        }
        publishSnapshot();
    }

    private void notifyNativeNetworkChanged() {
        underlayRecoveryFuture = null;
        if (!desiredConnected || !connected) {
            return;
        }
        if (operationInProgress) {
            underlayRecoveryFuture =
                    worker.schedule(
                            this::notifyNativeNetworkChanged,
                            UNDERLAY_RECOVERY_DELAY_MILLIS,
                            TimeUnit.MILLISECONDS);
            return;
        }
        runtimeNetworkChangeRequests = increment(runtimeNetworkChangeRequests);
        recordDiagnosticEvent("underlay_recovery_requested");
        long requestSequence = runtimeNetworkChangeRequests;
        Log.i(LOG_TAG, "event=underlay_runtime_recovery_requested sequence=" + requestSequence);
        try {
            NativeResponse.objectValue(NativeBridge.nativeNetworkChanged());
            underlayRecoveryPolicy.reset();
            recordDiagnosticEvent("underlay_recovery_completed");
            Log.i(LOG_TAG, "event=underlay_runtime_recovery_completed sequence=" + requestSequence);
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            runtimeNetworkChangeFailures = increment(runtimeNetworkChangeFailures);
            recordDiagnosticEvent("underlay_recovery_failed");
            connectionDetail = "Network recovery signal failed: " + failureMessage(error);
            Log.w(LOG_TAG, "event=underlay_runtime_recovery_failed sequence=" + requestSequence);
            UnderlayRecoveryPolicy.FailureAction action =
                    underlayRecoveryPolicy.recordSignalFailure();
            if (action == UnderlayRecoveryPolicy.FailureAction.RETRY_SIGNAL
                    && desiredConnected
                    && connected) {
                recordDiagnosticEvent("underlay_recovery_retry_scheduled");
                underlayRecoveryFuture =
                        worker.schedule(
                                this::notifyNativeNetworkChanged,
                                UNDERLAY_RECOVERY_DELAY_MILLIS,
                                TimeUnit.MILLISECONDS);
            } else {
                recordDiagnosticEvent("underlay_recovery_restart_scheduled");
                scheduleReconnect("Recovering after network signal failure", true);
            }
        }
        publishSnapshot();
    }

    private void scheduleStatusPoll() {
        cancel(statusFuture);
        statusFuture =
                worker.schedule(this::pollNativeStatus, STATUS_POLL_MILLIS, TimeUnit.MILLISECONDS);
    }

    private void scheduleBlockedModePoll() {
        cancel(statusFuture);
        statusFuture =
                worker.schedule(
                        this::pollNativeStatus,
                        BLOCKED_MODE_POLL_MILLIS,
                        TimeUnit.MILLISECONDS);
    }

    private void pollNativeStatus() {
        statusFuture = null;
        refreshVpnMode(false);
        if (!vpnMode.permitsOverlayConnection()) {
            if (connected) {
                stopNativeRuntime();
            }
            reportLockdownBlocked();
            if (desiredConnected) {
                scheduleBlockedModePoll();
            }
            return;
        }
        if (!connected) {
            if (desiredConnected) {
                startConnection("Restoring after blocked connections were disabled");
            }
            return;
        }
        if (!LocalNetworkPermission.isGranted(this)) {
            stopForMissingLocalNetworkPermission();
            return;
        }
        boolean runtimeFailed = false;
        String failure = null;
        try {
            JSONObject status = NativeResponse.objectValue(NativeBridge.nativeStatus());
            RuntimeStatusSnapshot runtimeStatus =
                    RuntimeStatusSnapshot.from(status, activeNetworkIds);
            networkRuntimeStatuses = runtimeStatus.networks;
            if (runtimeStatus.requiresWholeRuntimeRestart()) {
                runtimeFailed = true;
                failure =
                        runtimeStatus.detail.isEmpty()
                                ? "Native runtime stopped"
                                : runtimeStatus.detail;
            } else {
                connectionDetail = runtimeStatus.describeConnection();
            }
            List<String> metrics = runtimeStatus.metrics;
            latestRuntimeSummary =
                    runtimeSummaryAccumulator.observe(RuntimeSummary.fromLines(metrics));
            latestRuntimeDiagnostics = RuntimeDiagnostics.fromLines(metrics);
            peerDetail = latestRuntimeSummary.describe();
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            runtimeFailed = true;
            failure = failureMessage(error);
        }
        if (runtimeFailed) {
            recordDiagnosticEvent("runtime_health_failed");
            stopNativeRuntime();
            connectionDetail = failure;
            updateForegroundNotification();
            publishSnapshot();
            scheduleReconnect("Recovering native runtime", false);
            return;
        }
        publishSnapshot();
        if (connected) {
            scheduleStatusPoll();
        }
    }

    private void reportLockdownBlocked() {
        String detail = getString(R.string.lockdown_unsupported);
        if (!detail.equals(connectionDetail)) {
            recordDiagnosticEvent("lockdown_connection_blocked");
        }
        connectionDetail = detail;
        updateForegroundNotification();
        publishSnapshot();
    }

    private void stopForMissingLocalNetworkPermission() {
        recordDiagnosticEvent("local_network_permission_missing");
        boolean remainStarted = vpnMode.alwaysOn;
        if (!remainStarted) {
            desiredConnected = false;
        }
        if (connected) {
            stopNativeRuntime();
        }
        connectionDetail = "Local network permission is required";
        peerDetail = "Overlay peers: unavailable";
        updateForegroundNotification();
        publishSnapshot();
        if (remainStarted) {
            return;
        }
        mainHandler.post(
                () -> {
                    stopForeground(STOP_FOREGROUND_REMOVE);
                    stopSelf();
                });
    }

    private void loadProfileMetadata() {
        profilePresent = profileStore.exists();
        try {
            if (profilePresent) {
                loadProfile();
            }
        } catch (P2pVpnException | RuntimeException error) {
            profileUnreadable = true;
            connectionDetail = failureMessage(error);
        } catch (LinkageError error) {
            connectionDetail = failureMessage(error);
        }
        if (profile != null) {
            recordDiagnosticEvent("profile_loaded");
        } else if (profileUnreadable) {
            recordDiagnosticEvent("profile_unreadable");
        } else {
            recordDiagnosticEvent("profile_absent");
        }
        try {
            if (profileStore.exists() && profileStore.pairingExists()) {
                String legacyNetworkId =
                        profileCollection != null && profileCollection.networks.size() == 1
                                ? selectedNetworkId
                                : null;
                activePairing =
                        ActivePairing.fromJson(
                                profileStore.loadPairing(), legacyNetworkId);
                ProfileCollection.Entry pairingNetwork =
                        profileCollection == null
                                ? null
                                : profileCollection.find(activePairing.networkId);
                if (pairingNetwork == null || !pairingNetwork.enabled) {
                    throw new P2pVpnException(
                            "Saved pairing operation belongs to an inactive network");
                }
                if (activePairing.needsMigration) {
                    persistActivePairing();
                }
                pairingDetail = "Interrupted pairing will resume after connecting";
            } else if (profileStore.pairingExists()) {
                profileStore.clearPairing();
            }
        } catch (P2pVpnException | RuntimeException error) {
            profileStore.clearPairing();
            pairingDetail = "Discarded an unreadable pairing operation";
        }
        publishSnapshot();
    }

    private void loadProfile() throws P2pVpnException {
        String stored = profileStore.load();
        ProfileCollection.Decoded decoded = ProfileCollection.decode(stored);
        ProfileCollection loadedCollection;
        Map<String, AndroidProfile> loadedProfiles = new LinkedHashMap<>();
        boolean migrated = decoded.needsMigration();
        switch (decoded.state) {
            case LEGACY_PROFILE:
                String legacyConfigJson = decoded.legacyConfigJson();
                AndroidProfile legacyProfile = inspectProfile(legacyConfigJson);
                loadedCollection =
                        ProfileCollection.migrated(
                                legacyConfigJson,
                                legacyProfile.networkName,
                                legacyProfile.peerId,
                                ProfileCollection.PresentationAddresses.fromProfile(
                                        legacyProfile));
                loadedProfiles.put(loadedCollection.selectedNetworkId, legacyProfile);
                break;
            case SCHEMA_V1:
                ProfileCollection.SchemaV1Collection schemaV1 = decoded.schemaV1Collection();
                for (ProfileCollection.Entry network : schemaV1.networks) {
                    loadedProfiles.put(network.id, inspectProfile(network.configJson));
                }
                AndroidProfile selectedV1 = loadedProfiles.get(schemaV1.selectedNetworkId);
                if (selectedV1 == null) {
                    throw new P2pVpnException("Selected network profile is unavailable");
                }
                loadedCollection =
                        schemaV1.migrate(
                                ProfileCollection.PresentationAddresses.fromProfile(selectedV1));
                break;
            case SCHEMA_V2:
                ProfileCollection.SchemaV2Collection schemaV2 = decoded.schemaV2Collection();
                for (ProfileCollection.Entry network : schemaV2.networks) {
                    loadedProfiles.put(network.id, inspectProfile(network.configJson));
                }
                loadedCollection = schemaV2.migrate();
                break;
            case CURRENT:
                loadedCollection = decoded.currentCollection();
                for (ProfileCollection.Entry network : loadedCollection.networks) {
                    loadedProfiles.put(network.id, inspectProfile(network.configJson));
                }
                break;
            default:
                throw new P2pVpnException("Stored profile collection has an unsupported schema");
        }
        validateProfileCollection(loadedCollection, loadedProfiles);
        AndroidProfile selected = requireSelectedProfile(loadedCollection, loadedProfiles);
        Map<String, RuntimeFiles> loadedRuntimeFiles;
        try {
            loadedRuntimeFiles = prepareRuntimeFiles(loadedCollection);
        } catch (IOException error) {
            throw new P2pVpnException("Failed to prepare network runtime storage", error);
        }
        if (migrated) {
            profileStore.save(loadedCollection.toJson());
            recordDiagnosticEvent("profile_collection_migrated");
        }
        assignLoadedProfile(loadedCollection, loadedProfiles, loadedRuntimeFiles, selected);
    }

    private static AndroidProfile inspectProfile(String configJson) throws P2pVpnException {
        return AndroidProfile.fromNative(
                NativeResponse.objectValue(NativeBridge.nativeInspectProfile(configJson)));
    }

    private static void validateProfileCollection(
            ProfileCollection collection, Map<String, AndroidProfile> profiles)
            throws P2pVpnException {
        if (profiles.size() != collection.networks.size()) {
            throw new P2pVpnException("Profile collection inspection is incomplete");
        }
        Set<String> networkNames = new HashSet<>();
        Set<String> peerIds = new HashSet<>();
        for (ProfileCollection.Entry network : collection.networks) {
            AndroidProfile inspected = profiles.get(network.id);
            if (inspected == null) {
                throw new P2pVpnException("Profile collection contains an uninspected network");
            }
            if (!networkNames.add(inspected.networkName.toLowerCase(Locale.ROOT))) {
                throw new P2pVpnException("Profile collection contains duplicate network names");
            }
            if (!peerIds.add(inspected.peerId)) {
                throw new P2pVpnException("Profile collection reuses an identity across networks");
            }
        }
    }

    private static boolean hasEnabledNetwork(ProfileCollection collection) {
        if (collection == null) {
            return false;
        }
        for (ProfileCollection.Entry network : collection.networks) {
            if (network.enabled) {
                return true;
            }
        }
        return false;
    }

    private static int enabledNetworkCount(ProfileCollection collection) {
        if (collection == null) {
            return 0;
        }
        int enabled = 0;
        for (ProfileCollection.Entry network : collection.networks) {
            if (network.enabled) {
                enabled++;
            }
        }
        return enabled;
    }

    private static AndroidProfile requireSelectedProfile(
            ProfileCollection collection, Map<String, AndroidProfile> profiles)
            throws P2pVpnException {
        AndroidProfile selected = profiles.get(collection.selectedNetworkId);
        if (selected == null) {
            throw new P2pVpnException("Selected network profile is unavailable");
        }
        return selected;
    }

    private void assignLoadedProfile(
            ProfileCollection collection,
            Map<String, AndroidProfile> profiles,
            Map<String, RuntimeFiles> loadedRuntimeFiles,
            AndroidProfile selected) {
        profileCollection = collection;
        inspectedProfiles = Collections.unmodifiableMap(new LinkedHashMap<>(profiles));
        runtimeFiles = Collections.unmodifiableMap(new LinkedHashMap<>(loadedRuntimeFiles));
        selectedNetworkId = collection.selectedNetworkId;
        profile = selected;
    }

    private boolean beginNetworkMutation(String detail) {
        if (operationInProgress) {
            return false;
        }
        if (profileUnreadable) {
            connectionDetail = "Reset the unreadable profile before changing networks";
            publishSnapshot();
            return false;
        }
        if (activePairing != null) {
            connectionDetail = "Finish pairing before changing networks";
            publishSnapshot();
            return false;
        }
        operationInProgress = true;
        connectionDetail = detail;
        publishSnapshot();
        return true;
    }

    private void persistProfileCollection(
            ProfileCollection collection,
            Map<String, AndroidProfile> profiles,
            Map<String, RuntimeFiles> files)
            throws P2pVpnException {
        validateProfileCollection(collection, profiles);
        AndroidProfile selected = requireSelectedProfile(collection, profiles);
        if (hasEnabledNetwork(collection)) {
            AndroidRuntimePlan runtimePlan =
                    AndroidRuntimePlan.create(
                            collection, profiles, runtimeStatePaths(files));
            NativeResponse.objectValue(
                    NativeBridge.nativeValidateStartNetworks(runtimePlan.requestJson));
        }
        profileStore.save(collection.toJson());
        assignLoadedProfile(collection, profiles, files, selected);
        profilePresent = true;
        profileUnreadable = false;
    }

    private boolean suspendConnectionForNetworkChange() {
        if (!desiredConnected) {
            return false;
        }
        cancel(reconnectFuture);
        reconnectFuture = null;
        cancel(underlayRecoveryFuture);
        underlayRecoveryFuture = null;
        if (connected) {
            stopNativeRuntime();
        }
        return true;
    }

    private void createProfile(String networkName) {
        createProfile(networkName, true, null, null, null, null, null, null, null);
    }

    private void createUserProfile(String networkName) {
        createProfile(networkName, false, null, null, null, null, null, null, null);
    }

    private void createE2eProfile(String encodedSettings) {
        try {
            JSONObject settings = new JSONObject(encodedSettings);
            String packetQuicListen =
                    optionalDebugSetting(
                            settings,
                            "packet_quic_listen",
                            DEBUG_PACKET_QUIC_ENDPOINT_MAX_LENGTH);
            String packetQuicExternalEndpoint =
                    optionalDebugSetting(
                            settings,
                            "packet_quic_external_endpoint",
                            DEBUG_PACKET_QUIC_ENDPOINT_MAX_LENGTH);
            String relayReservation =
                    optionalDebugSetting(
                            settings,
                            "relay_reservation",
                            DEBUG_RELAY_RESERVATION_MAX_LENGTH);
            String additionalRoute =
                    optionalDebugSetting(
                            settings,
                            "additional_route",
                            DEBUG_ADDITIONAL_ROUTE_MAX_LENGTH);
            validateDebugE2ePaths(
                    packetQuicListen, packetQuicExternalEndpoint, relayReservation);
            createProfile(
                    requiredDebugSetting(settings, "network", 128),
                    true,
                    requiredDebugSetting(settings, "bootstrap_peer_id", 256),
                    requiredDebugSetting(settings, "bootstrap_address", 1_024),
                    requiredDebugSetting(settings, "kademlia_protocol", 128),
                    packetQuicListen,
                    packetQuicExternalEndpoint,
                    relayReservation,
                    additionalRoute);
        } catch (P2pVpnException | JSONException | RuntimeException error) {
            connectionDetail = failureMessage(error);
            publishSnapshot();
        }
    }

    private void setNetworkEnabledFromDebug(String encodedSettings) {
        try {
            JSONObject settings = new JSONObject(encodedSettings);
            setNetworkEnabled(
                    settings.getString("network_id"), settings.getBoolean("enabled"));
        } catch (JSONException | RuntimeException error) {
            connectionDetail = failureMessage(error);
            publishSnapshot();
        }
    }

    private void createProfile(
            String networkName,
            boolean enabledOnCreate,
            String bootstrapPeerId,
            String bootstrapAddress,
            String kademliaProtocol,
            String packetQuicListen,
            String packetQuicExternalEndpoint,
            String relayReservation,
            String additionalRoute) {
        if (!beginNetworkMutation(
                profileCollection == null ? "Creating profile" : "Adding network")) {
            return;
        }
        boolean resumeConnection = false;
        File uncommittedRuntimeDirectory = null;
        boolean profileCommitted = false;
        try {
            if (profileStore.exists() && profileCollection == null) {
                throw new P2pVpnException("The saved p2p-vpn profile is unavailable");
            }
            AndroidProfile created =
                    AndroidProfile.fromNative(
                            NativeResponse.objectValue(
                                    bootstrapPeerId == null
                                            ? NativeBridge.nativeCreateProfile(
                                                    networkName.trim(),
                                                    DeviceHostname.resolve(this))
                                            : NativeBridge.nativeCreateE2eProfile(
                                                    networkName.trim(),
                                                    bootstrapPeerId,
                                                    bootstrapAddress,
                                                    kademliaProtocol,
                                                    packetQuicListen,
                                                    packetQuicExternalEndpoint,
                                                    relayReservation,
                                                    additionalRoute)));
            ProfileCollection.Entry network =
                    new ProfileCollection.Entry(
                            ProfileCollection.newNetworkId(), enabledOnCreate, created.configJson);
            ProfileCollection collection =
                    profileCollection == null
                            ? ProfileCollection.single(
                                    network,
                                    ProfileCollection.PresentationAddresses.fromProfile(created))
                            : profileCollection.add(network, true);
            Map<String, AndroidProfile> profiles = new LinkedHashMap<>(inspectedProfiles);
            profiles.put(network.id, created);
            Map<String, RuntimeFiles> createdRuntimeFiles = prepareRuntimeFiles(collection);
            RuntimeFiles createdFiles = createdRuntimeFiles.get(network.id);
            if (createdFiles == null) {
                throw new IOException("failed to prepare new network runtime storage");
            }
            uncommittedRuntimeDirectory = createdFiles.directory;
            boolean firstNetwork = profileCollection == null;
            persistProfileCollection(collection, profiles, createdRuntimeFiles);
            profileCommitted = true;
            recordDiagnosticEvent(firstNetwork ? "profile_created" : "network_added");
            if (enabledOnCreate) {
                resumeConnection = suspendConnectionForNetworkChange();
            }
            connectionDetail = firstNetwork ? "Profile ready" : "Network added";
        } catch (P2pVpnException | IOException | RuntimeException | LinkageError error) {
            if (!profileCommitted && uncommittedRuntimeDirectory != null) {
                try {
                    deleteRuntimeEntry(uncommittedRuntimeDirectory);
                } catch (IOException cleanupError) {
                    Log.w(LOG_TAG, "event=uncommitted_network_cleanup_failed");
                }
            }
            connectionDetail = failureMessage(error);
        } finally {
            operationInProgress = false;
            publishSnapshot();
            if (resumeConnection && desiredConnected) {
                startConnection("Applying network changes");
            }
        }
    }

    private void joinProfileByCode(String pairingCode, String requestedHostname) {
        if (operationInProgress || profileJoinOperation != null || activePairing != null) {
            pairingDetail = "A pairing operation is already active";
            publishSnapshot();
            return;
        }
        if (profileUnreadable) {
            pairingDetail = "Reset the unreadable profile before joining a network";
            publishSnapshot();
            finishPairingForegroundService();
            return;
        }
        if (profileCollection != null
                && profileCollection.networks.size() >= ProfileCollection.MAX_NETWORKS) {
            pairingDetail = "This device already has the maximum number of networks";
            publishSnapshot();
            finishPairingForegroundService();
            return;
        }
        ProfileJoinRequest request;
        try {
            request =
                    ProfileJoinRequest.create(
                            pairingCode, requestedHostname, existingNetworkNames());
        } catch (P2pVpnException error) {
            pairingDetail = failureMessage(error);
            publishSnapshot();
            finishPairingForegroundService();
            return;
        }

        ProfileJoinOperation operation =
                new ProfileJoinOperation(PairingOperationId.generate());
        String candidateHintsJson = ProfileJoinDiscoveryHints.consumeNextForDebug();
        profileJoinOperation = operation;
        operationInProgress = true;
        pairingDetail = "Searching on LAN, then through public libp2p discovery";
        connectionDetail = "Joining a network";
        acquirePairingMulticastLock();
        publishSnapshot();
        profileJoinWorker.submit(
                () -> {
                    ProfileJoinResult result;
                    try {
                        AndroidProfile joined =
                                AndroidProfile.fromNative(
                                        NativeResponse.objectValue(
                                                NativeBridge.nativeJoinProfileByCode(
                                                        operation.id,
                                                        request.pairingCode,
                                                        request.hostname,
                                                        request.existingNetworkNamesJson,
                                                        candidateHintsJson)));
                        result = ProfileJoinResult.success(joined);
                    } catch (P2pVpnException | RuntimeException | LinkageError error) {
                        result = ProfileJoinResult.failure(failureMessage(error));
                    }
                    ProfileJoinResult completed = result;
                    try {
                        worker.execute(() -> completeProfileJoin(operation.id, completed));
                    } catch (RejectedExecutionException ignored) {
                        // Service destruction already cancelled the native operation.
                    }
                });
    }

    private List<String> existingNetworkNames() {
        List<String> names = new ArrayList<>(inspectedProfiles.size());
        for (AndroidProfile existing : inspectedProfiles.values()) {
            names.add(existing.networkName);
        }
        return names;
    }

    private void completeProfileJoin(String operationId, ProfileJoinResult result) {
        ProfileJoinOperation operation = profileJoinOperation;
        if (operation == null || !operation.id.equals(operationId)) {
            return;
        }
        File uncommittedRuntimeDirectory = null;
        boolean profileCommitted = false;
        try {
            if (result.error != null) {
                throw new P2pVpnException(result.error);
            }
            AndroidProfile joined = result.profile;
            if (joined == null) {
                throw new P2pVpnException("Pairing did not return a network profile");
            }
            if (profileCollection != null
                    && profileCollection.networks.size() >= ProfileCollection.MAX_NETWORKS) {
                throw new P2pVpnException(
                        "This device already has the maximum number of networks");
            }
            ProfileCollection.Entry network =
                    new ProfileCollection.Entry(
                            ProfileCollection.newNetworkId(), false, joined.configJson);
            ProfileCollection collection =
                    profileCollection == null
                            ? ProfileCollection.single(
                                    network,
                                    ProfileCollection.PresentationAddresses.fromProfile(joined))
                            : profileCollection.add(network, true);
            Map<String, AndroidProfile> profiles = new LinkedHashMap<>(inspectedProfiles);
            profiles.put(network.id, joined);
            Map<String, RuntimeFiles> joinedRuntimeFiles = prepareRuntimeFiles(collection);
            RuntimeFiles joinedFiles = joinedRuntimeFiles.get(network.id);
            if (joinedFiles == null) {
                throw new IOException("failed to prepare joined network runtime storage");
            }
            uncommittedRuntimeDirectory = joinedFiles.directory;
            boolean firstNetwork = profileCollection == null;
            persistProfileCollection(collection, profiles, joinedRuntimeFiles);
            profileCommitted = true;
            recordDiagnosticEvent(firstNetwork ? "profile_joined" : "network_joined");
            connectionDetail = firstNetwork ? "Profile ready" : "Network added";
            pairingDetail = "Joined " + joined.networkName;
        } catch (P2pVpnException | IOException | RuntimeException | LinkageError error) {
            if (!profileCommitted && uncommittedRuntimeDirectory != null) {
                try {
                    deleteRuntimeEntry(uncommittedRuntimeDirectory);
                } catch (IOException cleanupError) {
                    Log.w(LOG_TAG, "event=uncommitted_join_cleanup_failed");
                }
            }
            pairingDetail = failureMessage(error);
            connectionDetail = "Network join failed";
            recordDiagnosticEvent("profile_join_failed");
        } finally {
            profileJoinOperation = null;
            operationInProgress = false;
            releasePairingMulticastLock();
            publishSnapshot();
            finishPairingForegroundService();
        }
    }

    private void cancelProfileJoin() {
        ProfileJoinOperation operation = profileJoinOperation;
        if (operation == null) {
            pairingDetail = "No profile join is active";
            publishSnapshot();
            finishPairingForegroundService();
            return;
        }
        pairingDetail = "Cancelling network join";
        publishSnapshot();
        try {
            NativeResponse.objectValue(NativeBridge.nativeCancelProfileJoin(operation.id));
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            pairingDetail = failureMessage(error);
            publishSnapshot();
        }
    }

    private void cancelProfileJoinBestEffort() {
        ProfileJoinOperation operation = profileJoinOperation;
        if (operation == null) {
            return;
        }
        try {
            NativeBridge.nativeCancelProfileJoin(operation.id);
        } catch (RuntimeException | LinkageError ignored) {
            // Process teardown will close the profile-free libp2p swarm.
        }
    }

    private void finishPairingForegroundService() {
        refreshVpnMode(false);
        if (desiredConnected || vpnMode.alwaysOn) {
            updateForegroundNotification();
            return;
        }
        mainHandler.post(
                () -> {
                    stopForeground(STOP_FOREGROUND_REMOVE);
                    stopSelf();
                });
    }

    private void selectNetwork(String networkId) {
        if (!beginNetworkMutation("Selecting network")) {
            return;
        }
        try {
            if (profileCollection == null) {
                throw new P2pVpnException("No p2p-vpn network is available");
            }
            String normalized = ProfileCollection.Entry.normalizeNetworkId(networkId);
            ProfileCollection updated = profileCollection.select(normalized);
            persistProfileCollection(updated, inspectedProfiles, runtimeFiles);
            recordDiagnosticEvent("network_selected");
            connectionDetail = "Selected " + profile.networkName;
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            connectionDetail = failureMessage(error);
        } finally {
            operationInProgress = false;
            publishSnapshot();
        }
    }

    private void setNetworkEnabled(String networkId, boolean enabled) {
        if (!beginNetworkMutation(enabled ? "Enabling network" : "Disabling network")) {
            return;
        }
        boolean mutationSucceeded = false;
        try {
            if (profileCollection == null) {
                throw new P2pVpnException("No p2p-vpn network is available");
            }
            String normalized = ProfileCollection.Entry.normalizeNetworkId(networkId);
            ProfileCollection.Entry network = profileCollection.find(normalized);
            if (network == null) {
                throw new P2pVpnException("Cannot update an unknown network");
            }
            if (network.enabled != enabled) {
                ProfileCollection updated =
                        profileCollection.replace(network.withEnabled(enabled));
                persistProfileCollection(updated, inspectedProfiles, runtimeFiles);
                recordDiagnosticEvent(enabled ? "network_enabled" : "network_disabled");
                suspendConnectionForNetworkChange();
            }
            connectionDetail =
                    (enabled ? "Enabled " : "Disabled ")
                            + profileFor(normalized).networkName;
            mutationSucceeded = true;
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            connectionDetail = failureMessage(error);
        } finally {
            operationInProgress = false;
            publishSnapshot();
            if (mutationSucceeded) {
                reconcileEnabledNetworks();
            } else if (!desiredConnected && !vpnMode.alwaysOn) {
                stopManualService();
            }
        }
    }

    private void renameNetwork(String networkId, String hostname) {
        if (!beginNetworkMutation("Updating hostname")) {
            return;
        }
        boolean resumeConnection = false;
        boolean mutationSucceeded = false;
        try {
            if (profileCollection == null) {
                throw new P2pVpnException("No p2p-vpn network is available");
            }
            String normalized = ProfileCollection.Entry.normalizeNetworkId(networkId);
            ProfileCollection.Entry network = profileCollection.find(normalized);
            if (network == null) {
                throw new P2pVpnException("Cannot update an unknown network");
            }
            AndroidProfile current = profileFor(normalized);
            AndroidProfile renamed =
                    AndroidProfile.fromNative(
                            NativeResponse.objectValue(
                                    NativeBridge.nativeRenameProfile(
                                            network.configJson, hostname.trim())));
            if (!renamed.peerId.equals(current.peerId)) {
                throw new P2pVpnException("Hostname update changed the network identity");
            }
            if (!renamed.hostname.equals(current.hostname)) {
                Map<String, AndroidProfile> profiles = new LinkedHashMap<>(inspectedProfiles);
                profiles.put(normalized, renamed);
                ProfileCollection updated =
                        profileCollection.replace(network.withConfig(renamed.configJson));
                persistProfileCollection(updated, Collections.unmodifiableMap(profiles), runtimeFiles);
                if (network.enabled) {
                    resumeConnection = suspendConnectionForNetworkChange();
                }
                recordDiagnosticEvent("network_hostname_updated");
            }
            connectionDetail = "Hostname set to " + renamed.hostname;
            mutationSucceeded = true;
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            connectionDetail = failureMessage(error);
        } finally {
            operationInProgress = false;
            publishSnapshot();
            if (mutationSucceeded && resumeConnection) {
                connectRequested(false);
            }
        }
    }

    private void reconcileEnabledNetworks() {
        refreshVpnMode(false);
        NetworkActivationPolicy.Outcome outcome =
                NetworkActivationPolicy.afterMutation(
                        enabledNetworkCount(profileCollection), vpnMode.alwaysOn);
        if (outcome == NetworkActivationPolicy.Outcome.CONNECT) {
            connectRequested(false);
            return;
        }
        if (outcome == NetworkActivationPolicy.Outcome.IDLE_ALWAYS_ON) {
            desiredConnected = true;
            cancel(reconnectFuture);
            reconnectFuture = null;
            cancel(underlayRecoveryFuture);
            underlayRecoveryFuture = null;
            if (connected) {
                stopNativeRuntime();
            }
            connectionDetail = "No networks enabled";
            peerDetail = "Overlay peers: unavailable";
            updateForegroundNotification();
            publishSnapshot();
            return;
        }
        disconnectRequested(true);
    }

    private void stopManualService() {
        mainHandler.post(
                () -> {
                    stopForeground(STOP_FOREGROUND_REMOVE);
                    stopSelf();
                });
    }

    private void removeNetwork(String networkId) {
        if (!beginNetworkMutation("Removing network")) {
            return;
        }
        boolean resumeConnection = false;
        boolean removalSucceeded = false;
        try {
            if (profileCollection == null) {
                throw new P2pVpnException("No p2p-vpn network is available");
            }
            String normalized = ProfileCollection.Entry.normalizeNetworkId(networkId);
            ProfileCollection.Entry removedNetwork = profileCollection.find(normalized);
            if (removedNetwork == null) {
                throw new P2pVpnException("Cannot remove an unknown network");
            }
            AndroidProfile removedProfile = profileFor(normalized);
            RuntimeFiles removedFiles = runtimeFiles.get(normalized);
            if (removedFiles == null) {
                throw new P2pVpnException("Network runtime storage is unavailable");
            }
            if (profileCollection.networks.size() == 1) {
                resumeConnection = suspendConnectionForNetworkChange();
                deleteRuntimeState();
                profileStore.reset();
                profile = null;
                profileCollection = null;
                inspectedProfiles = Collections.emptyMap();
                runtimeFiles = Collections.emptyMap();
                selectedNetworkId = null;
                activeNetworkIds = Collections.emptyList();
                networkRuntimeStatuses = Collections.emptyMap();
                profilePresent = false;
                profileUnreadable = false;
                pairingDetail = "No pairing operation";
                recordDiagnosticEvent("last_network_removed");
                connectionDetail = "Removed " + removedProfile.networkName;
                removalSucceeded = true;
                return;
            }
            ProfileCollection updated = profileCollection.remove(normalized);
            Map<String, AndroidProfile> updatedProfiles = new LinkedHashMap<>(inspectedProfiles);
            updatedProfiles.remove(normalized);
            Map<String, RuntimeFiles> updatedFiles = new LinkedHashMap<>(runtimeFiles);
            updatedFiles.remove(normalized);
            persistProfileCollection(updated, updatedProfiles, updatedFiles);
            if (profileCollection.find(normalized) != null) {
                throw new P2pVpnException("Removed network remained in the profile collection");
            }
            if (removedNetwork.enabled) {
                resumeConnection = suspendConnectionForNetworkChange();
            }
            try {
                deleteRuntimeEntry(removedFiles.directory);
            } catch (IOException cleanupError) {
                Log.w(LOG_TAG, "event=removed_network_cleanup_failed");
            }
            recordDiagnosticEvent("network_removed");
            connectionDetail = "Removed " + removedProfile.networkName;
            removalSucceeded = true;
        } catch (P2pVpnException | IOException | RuntimeException | LinkageError error) {
            connectionDetail = failureMessage(error);
        } finally {
            operationInProgress = false;
            publishSnapshot();
            if (removalSucceeded) {
                reconcileEnabledNetworks();
            } else if (resumeConnection && desiredConnected) {
                startConnection("Applying network changes");
            }
        }
    }

    private AndroidProfile profileFor(String networkId) throws P2pVpnException {
        AndroidProfile result = inspectedProfiles.get(networkId);
        if (result == null) {
            throw new P2pVpnException("Network profile is unavailable");
        }
        return result;
    }

    private void executeDebugCommand(String command, String value) {
        switch (command) {
            case "ensure":
                publishSnapshot();
                break;
            case "create-profile":
                createProfile(value == null ? "" : value);
                break;
            case "create-e2e-profile":
                createE2eProfile(value == null ? "" : value);
                break;
            case "select-network":
                selectNetwork(value);
                break;
            case "set-network-enabled":
                setNetworkEnabledFromDebug(value == null ? "" : value);
                break;
            case "remove-network":
                removeNetwork(value);
                break;
            case "stage-legacy-profile":
                stageLegacyProfileForDebug();
                break;
            case "open-pairing":
                openPairing();
                break;
            case "join-pairing":
                joinPairing(value == null ? "" : value);
                break;
            case "approve-pairing":
                approvePairing(value);
                break;
            case "reject-pairing":
                rejectPairing();
                break;
            default:
                break;
        }
    }

    private void stageLegacyProfileForDebug() {
        if (!beginNetworkMutation("Staging legacy profile")) {
            return;
        }
        try {
            if (profileCollection == null || profileCollection.networks.size() != 1) {
                throw new P2pVpnException("Legacy migration requires exactly one network");
            }
            if (desiredConnected) {
                throw new P2pVpnException("Disconnect before staging a legacy profile");
            }
            RuntimeFiles currentFiles = runtimeFiles.get(selectedNetworkId);
            if (currentFiles == null) {
                throw new IOException("Legacy migration runtime storage is unavailable");
            }
            deleteRuntimeEntry(currentFiles.directory);
            profileStore.save(profile.configJson);
            recordDiagnosticEvent("legacy_profile_staged");
            connectionDetail = "Legacy profile staged for migration";
        } catch (P2pVpnException | IOException | RuntimeException error) {
            connectionDetail = failureMessage(error);
        } finally {
            operationInProgress = false;
            publishSnapshot();
        }
    }

    private void resetUnreadableProfile() {
        if (!profileUnreadable || desiredConnected || operationInProgress) {
            connectionDetail = "The unreadable profile cannot be reset right now";
            publishSnapshot();
            return;
        }
        operationInProgress = true;
        connectionDetail = "Removing unreadable profile";
        publishSnapshot();
        try {
            deleteRuntimeState();
            profileStore.reset();
            profile = null;
            profileCollection = null;
            inspectedProfiles = Collections.emptyMap();
            runtimeFiles = Collections.emptyMap();
            selectedNetworkId = null;
            activeNetworkIds = Collections.emptyList();
            networkRuntimeStatuses = Collections.emptyMap();
            profilePresent = false;
            profileUnreadable = false;
            activePairing = null;
            pairingDetail = "No pairing operation";
            recordDiagnosticEvent("profile_reset");
            connectionDetail = "Profile removed";
        } catch (P2pVpnException | IOException | RuntimeException error) {
            connectionDetail = failureMessage(error);
        } finally {
            operationInProgress = false;
            publishSnapshot();
        }
    }

    private void deleteRuntimeState() throws IOException {
        if (runtimeDirectory == null || !runtimeDirectory.exists()) {
            return;
        }
        File[] entries = runtimeDirectory.listFiles();
        if (entries == null) {
            throw new IOException("failed to list private runtime storage");
        }
        for (File entry : entries) {
            deleteRuntimeEntry(entry);
        }
        runtimeFiles = Collections.emptyMap();
    }

    private static void deleteRuntimeEntry(File entry) throws IOException {
        if (entry.isDirectory() && !Files.isSymbolicLink(entry.toPath())) {
            File[] children = entry.listFiles();
            if (children == null) {
                throw new IOException("failed to list network runtime storage");
            }
            for (File child : children) {
                deleteRuntimeEntry(child);
            }
        }
        Files.deleteIfExists(entry.toPath());
    }

    private void openPairing() {
        if (!requireConnectedForPairing()) {
            return;
        }
        try {
            activePairing =
                    ActivePairing.inviter(selectedNetworkId, PairingOperationId.generate());
            recordDiagnosticEvent("pairing_open_started");
            persistActivePairing();
            pairingDetail = "Creating a pairing code";
            publishSnapshot();
            resumeActivePairing();
        } catch (P2pVpnException | RuntimeException error) {
            clearActivePairing();
            pairingDetail = failureMessage(error);
            publishSnapshot();
        }
    }

    private void joinPairing(String code) {
        if (!requireConnectedForPairing()) {
            return;
        }
        String normalized = code.trim().toUpperCase(Locale.ROOT);
        if (normalized.isEmpty() || normalized.length() > 64) {
            pairingDetail = "Enter a valid pairing code";
            publishSnapshot();
            return;
        }
        try {
            activePairing =
                    ActivePairing.joiner(
                            selectedNetworkId, PairingOperationId.generate(), normalized);
            recordDiagnosticEvent("pairing_join_started");
            persistActivePairing();
            pairingDetail = "Starting the join operation";
            publishSnapshot();
            resumeActivePairing();
        } catch (P2pVpnException | RuntimeException error) {
            clearActivePairing();
            pairingDetail = failureMessage(error);
            publishSnapshot();
        }
    }

    private void resumeActivePairing() {
        pairingFuture = null;
        if (activePairing == null || !connected) {
            return;
        }
        try {
            if (activePairing.transcriptSha256 != null) {
                acknowledgeAppliedPairing();
                return;
            }
            if (!activePairing.started) {
                PairRpc.Result result =
                        activePairing.role == ActivePairing.Role.INVITER
                                ? PairRpc.call(
                                        activePairing.networkId,
                                        PairRpc.open(
                                                activePairing.operationId,
                                                PAIRING_TIMEOUT_SECONDS))
                                : PairRpc.call(
                                        activePairing.networkId,
                                        PairRpc.join(
                                                activePairing.operationId,
                                                activePairing.code,
                                                PAIRING_TIMEOUT_SECONDS));
                String expectedKind =
                        activePairing.role == ActivePairing.Role.INVITER
                                ? "open_started"
                                : "join_started";
                if (!expectedKind.equals(result.kind)
                        || !activePairing.operationId.equals(
                                result.value.getString("operation_id"))) {
                    throw new P2pVpnException("Pairing RPC started a different operation");
                }
                if (activePairing.role == ActivePairing.Role.INVITER) {
                    activePairing.code = result.value.getString("code");
                }
                activePairing.started = true;
            }
            persistActivePairing();
            pairingDetail =
                    activePairing.role == ActivePairing.Role.INVITER
                            ? "Waiting for a pairing request"
                            : "Finding the inviting peer";
            publishSnapshot();
            schedulePairingPoll(0);
        } catch (P2pVpnException | JSONException | RuntimeException | LinkageError error) {
            pairingDetail = failureMessage(error);
            publishSnapshot();
            if (activePairing != null && connected) {
                schedulePairingResume(PAIRING_POLL_MILLIS);
            }
        }
    }

    private void approvePairing(String hostname) {
        if (activePairing == null || activePairing.candidate == null) {
            pairingDetail = "No pairing request is awaiting approval";
            publishSnapshot();
            return;
        }
        try {
            PairRpc.Result result =
                    PairRpc.call(
                            activePairing.networkId,
                            PairRpc.approve(
                                    activePairing.operationId,
                                    activePairing.candidate.approvalId,
                                    normalizeHostname(hostname)));
            PairRpc.operationStatus(result);
            activePairing.candidate = null;
            pairingDetail = "Finalizing pairing";
            publishSnapshot();
            schedulePairingPoll(0);
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            pairingDetail = failureMessage(error);
            publishSnapshot();
        }
    }

    private void rejectPairing() {
        if (activePairing == null || activePairing.candidate == null) {
            pairingDetail = "No pairing request is awaiting approval";
            publishSnapshot();
            return;
        }
        try {
            PairRpc.Result result =
                    PairRpc.call(
                            activePairing.networkId,
                            PairRpc.reject(
                                    activePairing.operationId,
                                    activePairing.candidate.approvalId));
            PairRpc.operationStatus(result);
            pairingDetail = "Pairing request rejected";
            recordDiagnosticEvent("pairing_rejected");
            clearActivePairing();
            cancel(pairingFuture);
            pairingFuture = null;
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            pairingDetail = failureMessage(error);
        }
        publishSnapshot();
    }

    private boolean requireConnectedForPairing() {
        if (!connected) {
            pairingDetail = "Connect before pairing";
            publishSnapshot();
            return false;
        }
        if (activePairing != null) {
            pairingDetail = "A pairing operation is already active";
            publishSnapshot();
            return false;
        }
        if (profileJoinOperation != null) {
            pairingDetail = "A profile-free pairing operation is already active";
            publishSnapshot();
            return false;
        }
        if (selectedNetworkId == null || !activeNetworkIds.contains(selectedNetworkId)) {
            pairingDetail = "Enable the selected network before pairing";
            publishSnapshot();
            return false;
        }
        NetworkRuntimeStatus selectedStatus = networkRuntimeStatuses.get(selectedNetworkId);
        if (selectedStatus != null && !selectedStatus.isAvailable()) {
            pairingDetail = "The selected network is not running";
            publishSnapshot();
            return false;
        }
        return true;
    }

    private void schedulePairingPoll(long delayMillis) {
        cancel(pairingFuture);
        pairingFuture =
                worker.schedule(this::pollPairing, delayMillis, TimeUnit.MILLISECONDS);
    }

    private void schedulePairingResume(long delayMillis) {
        cancel(pairingFuture);
        pairingFuture =
                worker.schedule(this::resumeActivePairing, delayMillis, TimeUnit.MILLISECONDS);
    }

    private void pollPairing() {
        pairingFuture = null;
        if (activePairing == null || !connected) {
            return;
        }
        if (activePairing.transcriptSha256 != null) {
            resumeActivePairing();
            return;
        }
        try {
            PairRpc.OperationStatus status =
                    PairRpc.operationStatus(
                            PairRpc.call(
                                    activePairing.networkId,
                                    PairRpc.status(activePairing.operationId)));
            if (!activePairing.operationId.equals(status.operationId)) {
                throw new P2pVpnException("Pairing status belongs to another operation");
            }
            EnrollmentDecision.Action action =
                    EnrollmentDecision.evaluate(
                            status.phase, status.artifactsReady, status.candidate != null);
            if (action == EnrollmentDecision.Action.APPLY_ARTIFACTS) {
                applyPairingArtifacts();
                return;
            }
            if (action == EnrollmentDecision.Action.AWAIT_APPROVAL) {
                activePairing.candidate = status.candidate;
                pairingDetail = "Review the pairing request";
            } else if (action == EnrollmentDecision.Action.TERMINAL) {
                pairingDetail =
                        status.failureMessage == null
                                ? "Pairing ended: " + status.phase
                                : status.failureMessage;
                clearActivePairing();
                publishSnapshot();
                return;
            } else {
                pairingDetail = pairingProgress(status);
            }
            publishSnapshot();
            schedulePairingPoll(PAIRING_POLL_MILLIS);
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            pairingDetail = failureMessage(error);
            publishSnapshot();
            if (activePairing != null && connected) {
                schedulePairingPoll(PAIRING_POLL_MILLIS);
            }
        }
    }

    private void applyPairingArtifacts() {
        if (activePairing == null) {
            return;
        }
        try {
            PairRpc.Result result =
                    PairRpc.call(
                            activePairing.networkId,
                            PairRpc.artifacts(activePairing.operationId));
            if (!"artifacts".equals(result.kind)) {
                throw new P2pVpnException("Pairing RPC did not return enrollment artifacts");
            }
            JSONObject artifacts = result.value;
            JSONObject receipt = artifacts.getJSONObject("receipt");
            String transcript = receipt.getString("transcript_sha256");
            String remotePeer = receipt.getString("remote_peer");

            if (profileCollection == null) {
                throw new P2pVpnException("Pairing network profile is unavailable");
            }
            ProfileCollection.Entry pairingNetwork =
                    profileCollection.find(activePairing.networkId);
            RuntimeFiles pairingRuntimeFiles = runtimeFiles.get(activePairing.networkId);
            if (pairingNetwork == null || pairingRuntimeFiles == null) {
                throw new P2pVpnException("Pairing network profile is unavailable");
            }
            AndroidProfile updated =
                    AndroidProfile.fromNative(
                            NativeResponse.objectValue(
                                    NativeBridge.nativeApplyPairingArtifacts(
                                            pairingNetwork.configJson,
                                            artifacts.toString(),
                                            pairingRuntimeFiles.directory.getAbsolutePath())));

            // Durably save the new identity bindings before compacting native enrollment state.
            ProfileCollection updatedCollection =
                    profileCollection.replace(pairingNetwork.withConfig(updated.configJson));
            profileStore.save(updatedCollection.toJson());
            Map<String, AndroidProfile> updatedProfiles = new LinkedHashMap<>(inspectedProfiles);
            updatedProfiles.put(activePairing.networkId, updated);
            profileCollection = updatedCollection;
            inspectedProfiles = Collections.unmodifiableMap(updatedProfiles);
            if (activePairing.networkId.equals(selectedNetworkId)) {
                profile = updated;
            }
            activePairing.transcriptSha256 = transcript;
            activePairing.remotePeer = remotePeer;
            persistActivePairing();
            acknowledgeAppliedPairing();
        } catch (P2pVpnException | JSONException | RuntimeException | LinkageError error) {
            pairingDetail = failureMessage(error);
            publishSnapshot();
            if (activePairing != null && connected) {
                schedulePairingPoll(PAIRING_POLL_MILLIS);
            }
        }
    }

    private void acknowledgeAppliedPairing()
            throws P2pVpnException, JSONException {
        if (activePairing == null || activePairing.transcriptSha256 == null) {
            throw new P2pVpnException("No applied pairing is awaiting acknowledgement");
        }
        PairRpc.Result acknowledged =
                PairRpc.call(
                        activePairing.networkId,
                        PairRpc.acknowledge(
                                activePairing.operationId, activePairing.transcriptSha256));
        if (!"acknowledged".equals(acknowledged.kind)) {
            throw new P2pVpnException("Pairing RPC did not acknowledge enrollment");
        }

        String remotePeer = activePairing.remotePeer;
        clearActivePairing();
        recordDiagnosticEvent("pairing_completed");
        pairingDetail = remotePeer == null ? "Pairing complete" : "Paired with " + remotePeer;
        publishSnapshot();

        if (desiredConnected) {
            operationInProgress = true;
            connectionDetail = "Restarting with paired profile";
            publishSnapshot();
            stopNativeRuntime();
            operationInProgress = false;
            startConnection("Restarting with paired profile");
        }
    }

    private void persistActivePairing() throws P2pVpnException {
        if (activePairing == null) {
            throw new P2pVpnException("No pairing operation is active");
        }
        profileStore.savePairing(activePairing.toJson());
    }

    private void cancelActivePairingBestEffort() {
        if (activePairing == null || !connected || !activePairing.started) {
            return;
        }
        try {
            PairRpc.call(
                    activePairing.networkId, PairRpc.cancel(activePairing.operationId));
        } catch (P2pVpnException | RuntimeException | LinkageError ignored) {
            // The runtime is stopping and durable pairing state expires independently.
        }
    }

    private void clearActivePairing() {
        activePairing = null;
        profileStore.clearPairing();
    }

    private void prepareRuntimeDirectory() {
        runtimeDirectory = new File(getNoBackupFilesDir(), "runtime");
        if (!preparePrivateDirectory(runtimeDirectory)) {
            connectionDetail = "Failed to create private runtime storage";
            return;
        }
        try {
            Files.deleteIfExists(new File(runtimeDirectory, "membership.key").toPath());
            runtimeStorageReady = true;
        } catch (IOException error) {
            connectionDetail = "Failed to remove a stale pairing secret";
        }
    }

    private Map<String, RuntimeFiles> prepareRuntimeFiles(ProfileCollection collection)
            throws IOException {
        Map<String, RuntimeFiles> prepared = new LinkedHashMap<>();
        for (ProfileCollection.Entry network : collection.networks) {
            File networkDirectory = new File(runtimeDirectory, network.id);
            if (!preparePrivateDirectory(networkDirectory)) {
                throw new IOException("failed to create private network runtime directory");
            }
            File pairingState = new File(networkDirectory, "pairing-state.json");
            File membershipState = new File(networkDirectory, "membership-state.json");
            validateRuntimeStateFile(pairingState);
            validateRuntimeStateFile(membershipState);
            if (network.id.equals(collection.selectedNetworkId)) {
                migrateRuntimeFile(new File(runtimeDirectory, "pairing-state.json"), pairingState);
                migrateRuntimeFile(
                        new File(runtimeDirectory, "membership-state.json"), membershipState);
            }
            Files.deleteIfExists(new File(networkDirectory, "membership.key").toPath());
            prepared.put(
                    network.id,
                    new RuntimeFiles(networkDirectory, pairingState, membershipState));
        }
        return prepared;
    }

    private Map<String, AndroidRuntimePlan.StatePaths> runtimeStatePaths()
            throws P2pVpnException {
        return runtimeStatePaths(runtimeFiles);
    }

    private static Map<String, AndroidRuntimePlan.StatePaths> runtimeStatePaths(
            Map<String, RuntimeFiles> filesByNetwork) throws P2pVpnException {
        Map<String, AndroidRuntimePlan.StatePaths> paths = new LinkedHashMap<>();
        for (Map.Entry<String, RuntimeFiles> entry : filesByNetwork.entrySet()) {
            RuntimeFiles files = entry.getValue();
            paths.put(
                    entry.getKey(),
                    new AndroidRuntimePlan.StatePaths(
                            files.directory.getAbsolutePath(),
                            files.pairingState.getAbsolutePath(),
                            files.membershipState.getAbsolutePath()));
        }
        return paths;
    }

    private static void validateRuntimeStateFile(File stateFile) throws IOException {
        if (Files.exists(stateFile.toPath(), LinkOption.NOFOLLOW_LINKS)
                && !Files.isRegularFile(stateFile.toPath(), LinkOption.NOFOLLOW_LINKS)) {
            throw new IOException("network runtime state is not a regular file");
        }
    }

    private static boolean preparePrivateDirectory(File directory) {
        if (Files.exists(directory.toPath(), LinkOption.NOFOLLOW_LINKS)) {
            if (!Files.isDirectory(directory.toPath(), LinkOption.NOFOLLOW_LINKS)) {
                return false;
            }
        } else if (!directory.mkdirs()) {
            return false;
        }
        directory.setReadable(false, false);
        directory.setWritable(false, false);
        directory.setExecutable(false, false);
        return directory.setReadable(true, true)
                && directory.setWritable(true, true)
                && directory.setExecutable(true, true);
    }

    private static void migrateRuntimeFile(File legacy, File target) throws IOException {
        if (!Files.exists(legacy.toPath(), LinkOption.NOFOLLOW_LINKS)) {
            return;
        }
        if (!Files.isRegularFile(legacy.toPath(), LinkOption.NOFOLLOW_LINKS)) {
            throw new IOException("legacy runtime state is not a regular file");
        }
        if (Files.exists(target.toPath(), LinkOption.NOFOLLOW_LINKS)) {
            throw new IOException("legacy and network runtime state both exist");
        }
        try {
            Files.move(legacy.toPath(), target.toPath(), StandardCopyOption.ATOMIC_MOVE);
        } catch (AtomicMoveNotSupportedException error) {
            Files.move(legacy.toPath(), target.toPath());
        }
    }

    private void acquireMulticastLock() {
        try {
            if (multicastLock != null && multicastLock.isHeld()) {
                return;
            }
            WifiManager wifi =
                    (WifiManager)
                            getApplicationContext().getSystemService(Context.WIFI_SERVICE);
            if (wifi == null) {
                return;
            }
            multicastLock = wifi.createMulticastLock("p2p-vpn-lan-discovery");
            multicastLock.setReferenceCounted(false);
            multicastLock.acquire();
        } catch (RuntimeException error) {
            // LAN discovery is an optimization; public discovery must remain available.
            releaseMulticastLock();
        }
    }

    private void releaseMulticastLock() {
        try {
            if (multicastLock != null && multicastLock.isHeld()) {
                multicastLock.release();
            }
        } catch (RuntimeException ignored) {
            // The Wi-Fi subsystem can disappear during a network transition.
        }
        multicastLock = null;
    }

    private void acquirePairingMulticastLock() {
        try {
            if (pairingMulticastLock != null && pairingMulticastLock.isHeld()) {
                return;
            }
            WifiManager wifi =
                    (WifiManager)
                            getApplicationContext().getSystemService(Context.WIFI_SERVICE);
            if (wifi == null) {
                return;
            }
            pairingMulticastLock = wifi.createMulticastLock("p2p-vpn-pairing-discovery");
            pairingMulticastLock.setReferenceCounted(false);
            pairingMulticastLock.acquire();
        } catch (RuntimeException error) {
            releasePairingMulticastLock();
        }
    }

    private void releasePairingMulticastLock() {
        try {
            if (pairingMulticastLock != null && pairingMulticastLock.isHeld()) {
                pairingMulticastLock.release();
            }
        } catch (RuntimeException ignored) {
            // Public pairing discovery remains available without Wi-Fi multicast.
        }
        pairingMulticastLock = null;
    }

    private void registerNetworkCallback() {
        connectivityManager =
                (ConnectivityManager) getSystemService(Context.CONNECTIVITY_SERVICE);
        if (connectivityManager == null) {
            return;
        }
        networkCallback =
                new ConnectivityManager.NetworkCallback() {
                    @Override
                    public void onCapabilitiesChanged(
                            Network network, NetworkCapabilities capabilities) {
                        if (isPhysicalNetwork(capabilities)) {
                            handlePhysicalNetwork(network, capabilities);
                        }
                    }

                    @Override
                    public void onLost(Network network) {
                        UnderlayTracker.Change change =
                                underlayTracker.lost(network.toString());
                        if (change != UnderlayTracker.Change.UNCHANGED) {
                            worker.execute(() -> handleUnderlayChange(change));
                        }
                    }
                };
        NetworkRequest request =
                new NetworkRequest.Builder()
                        .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                        .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
                        .build();
        connectivityManager.registerNetworkCallback(request, networkCallback, mainHandler);
    }

    private void handlePhysicalNetwork(Network network, NetworkCapabilities capabilities) {
        if (!isPhysicalNetwork(capabilities)) {
            return;
        }
        UnderlayTracker.Change change =
                underlayTracker.observe(
                        network.toString(),
                        physicalNetworkKind(capabilities),
                        capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED));
        if (change != UnderlayTracker.Change.UNCHANGED) {
            worker.execute(() -> handleUnderlayChange(change));
        }
    }

    private static boolean isPhysicalNetwork(NetworkCapabilities capabilities) {
        return capabilities != null
                && capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                && capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN);
    }

    private static UnderlayTracker.Kind physicalNetworkKind(
            NetworkCapabilities capabilities) {
        if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) {
            return UnderlayTracker.Kind.ETHERNET;
        }
        if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) {
            return UnderlayTracker.Kind.WIFI;
        }
        if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) {
            return UnderlayTracker.Kind.CELLULAR;
        }
        if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_BLUETOOTH)) {
            return UnderlayTracker.Kind.BLUETOOTH;
        }
        return UnderlayTracker.Kind.OTHER;
    }

    private void createNotificationChannel() {
        NotificationManager manager = getSystemService(NotificationManager.class);
        NotificationChannel channel =
                new NotificationChannel(
                        NOTIFICATION_CHANNEL,
                        getString(R.string.notification_channel),
                        NotificationManager.IMPORTANCE_LOW);
        channel.setDescription(getString(R.string.notification_channel_description));
        manager.createNotificationChannel(channel);
    }

    private Notification notification(String text) {
        Intent activity = new Intent(this, MainActivity.class);
        PendingIntent contentIntent =
                PendingIntent.getActivity(
                        this,
                        0,
                        activity,
                        PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Builder(this, NOTIFICATION_CHANNEL)
                .setSmallIcon(android.R.drawable.ic_lock_lock)
                .setContentTitle(getString(R.string.app_name))
                .setContentText(text)
                .setContentIntent(contentIntent)
                .setCategory(Notification.CATEGORY_SERVICE)
                .setOngoing(connected || desiredConnected)
                .setOnlyAlertOnce(true)
                .build();
    }

    private void updateForegroundNotification() {
        NotificationManager manager = getSystemService(NotificationManager.class);
        String message =
                connected && !activeNetworkIds.isEmpty()
                        ? getString(R.string.notification_connected, activeNetworkLabel())
                        : connectionDetail;
        manager.notify(NOTIFICATION_ID, notification(message));
    }

    private String activeNetworkLabel() {
        if (activeNetworkIds.size() == 1) {
            AndroidProfile active = inspectedProfiles.get(activeNetworkIds.get(0));
            if (active != null) {
                return active.networkName;
            }
        }
        return activeNetworkIds.size() + " networks";
    }

    private void publishSnapshot() {
        refreshVpnMode(false);
        PairRpc.Candidate candidate = activePairing == null ? null : activePairing.candidate;
        List<NetworkSnapshot> networks = networkSnapshots();
        snapshot =
                new Snapshot(
                        profile != null,
                        profilePresent,
                        profileUnreadable,
                        connected,
                        desiredConnected,
                        operationInProgress,
                        vpnMode.alwaysOn,
                        vpnMode.lockdown,
                        profile == null ? null : profile.networkName,
                        profile == null ? null : profile.hostname,
                        profile == null ? null : profile.peerId,
                        profileAddresses(profile),
                        selectedNetworkId,
                        networks,
                        latestRuntimeSummary,
                        latestRuntimeDiagnostics,
                        runtimeGeneration,
                        runtimeNetworkChangeRequests,
                        runtimeNetworkChangeFailures,
                        underlayTracker.snapshot(),
                        vpnModeDetail(),
                        connectionDetail,
                        peerDetail,
                        activePairing != null || profileJoinOperation != null,
                        profileJoinOperation != null,
                        pairingDetail,
                        activePairing == null ? null : activePairing.displayCode(),
                        candidate == null ? null : candidate.peerId,
                        candidate == null ? null : candidate.fingerprint,
                        candidate == null ? null : candidate.requestedHostname,
                        candidate == null ? null : candidate.requestedVpnIp);
        Snapshot current = snapshot;
        mainHandler.post(
                () -> {
                    for (Listener listener : listeners) {
                        listener.onSnapshot(current);
                    }
                });
    }

    private List<NetworkSnapshot> networkSnapshots() {
        return networkSnapshots(
                profileCollection,
                inspectedProfiles,
                networkRuntimeStatuses,
                desiredConnected,
                connected,
                connectionDetail);
    }

    static List<NetworkSnapshot> networkSnapshots(
            ProfileCollection collection,
            Map<String, AndroidProfile> profiles,
            Map<String, NetworkRuntimeStatus> runtimeStatuses,
            boolean connectionRequested,
            boolean connected,
            String connectionDetail) {
        if (collection == null) {
            return Collections.emptyList();
        }
        List<NetworkSnapshot> result = new ArrayList<>(collection.networks.size());
        for (ProfileCollection.Entry entry : collection.networks) {
            AndroidProfile inspected = profiles.get(entry.id);
            if (inspected == null) {
                continue;
            }
            NetworkRuntimeStatus runtimeStatus = runtimeStatuses.get(entry.id);
            String phase;
            String detail;
            if (!entry.enabled) {
                phase = "disabled";
                detail = "";
            } else if (runtimeStatus != null) {
                phase = runtimeStatus.phase;
                detail = runtimeStatus.detail;
            } else if (connectionRequested) {
                phase = "starting";
                detail = connected ? "Waiting for runtime status" : connectionDetail;
            } else {
                phase = "stopped";
                detail = "";
            }
            boolean peersAvailable =
                    entry.enabled
                            && runtimeStatus != null
                            && "running".equals(runtimeStatus.phase);
            PeerSnapshot peers =
                    peersAvailable ? runtimeStatus.peerSnapshot.orElse(null) : null;
            result.add(
                    new NetworkSnapshot(
                            entry.id,
                            inspected.networkName,
                            inspected.hostname,
                            inspected.peerId,
                            profileAddresses(inspected),
                            entry.enabled,
                            entry.id.equals(collection.selectedNetworkId),
                            phase,
                            detail,
                            peers));
        }
        return Collections.unmodifiableList(result);
    }

    private static List<String> profileAddresses(AndroidProfile currentProfile) {
        if (currentProfile == null) {
            return Collections.emptyList();
        }
        List<String> addresses = new ArrayList<>(currentProfile.addresses.size());
        for (AndroidProfile.Cidr address : currentProfile.addresses) {
            addresses.add(address.address + "/" + address.prefixLength);
        }
        return addresses;
    }

    private String createDiagnosticReport() {
        recordDiagnosticEvent("diagnostic_report_generated");
        Snapshot current = snapshot;
        UnderlayTracker.Snapshot underlay = current.underlay;
        long serviceUptime =
                Math.max(0, SystemClock.elapsedRealtime() - serviceStartedElapsedRealtime);
        Runtime javaRuntime = Runtime.getRuntime();
        Debug.MemoryInfo memory = new Debug.MemoryInfo();
        Debug.getMemoryInfo(memory);
        DiagnosticReport.Resources resources =
                new DiagnosticReport.Resources(
                        Process.getElapsedCpuTime(),
                        memory.getTotalPss(),
                        memory.getTotalPrivateDirty(),
                        javaRuntime.totalMemory() - javaRuntime.freeMemory(),
                        javaRuntime.maxMemory(),
                        Thread.activeCount());
        return DiagnosticReport.create(
                new DiagnosticReport.Input(
                        Instant.now().toString(),
                        appVersion(),
                        Build.VERSION.SDK_INT,
                        serviceUptime,
                        current.profileStored,
                        current.hasProfile && !current.profileUnreadable,
                        current.connectionRequested,
                        current.connected,
                        current.alwaysOn,
                        current.lockdown,
                        current.busy,
                        current.runtimeGeneration,
                        underlay.kind,
                        underlay.validated,
                        underlay.availableNetworks,
                        underlay.selectionChanges,
                        underlay.selectedLosses,
                        underlay.recoveries,
                        current.runtimeNetworkChangeRequests,
                        current.runtimeNetworkChangeFailures,
                        current.pairingActive,
                        current.candidatePeer != null,
                        current.runtimeSummary,
                        current.runtimeDiagnostics,
                        resources,
                        diagnosticEvents.snapshot()));
    }

    @SuppressWarnings("deprecation")
    private String appVersion() {
        try {
            PackageInfo info;
            if (Build.VERSION.SDK_INT >= 33) {
                info =
                        getPackageManager()
                                .getPackageInfo(
                                        getPackageName(),
                                        PackageManager.PackageInfoFlags.of(0));
            } else {
                info = getPackageManager().getPackageInfo(getPackageName(), 0);
            }
            return info.versionName == null ? "unknown" : info.versionName;
        } catch (PackageManager.NameNotFoundException error) {
            return "unknown";
        }
    }

    private void recordDiagnosticEvent(String name) {
        diagnosticEvents.record(
                name,
                Math.max(0, SystemClock.elapsedRealtime() - serviceStartedElapsedRealtime));
    }

    static Snapshot debugSnapshot() {
        P2pVpnService current = debugInstance;
        return current == null ? null : current.snapshot;
    }

    static String debugDiagnosticReport() {
        P2pVpnService current = debugInstance;
        return current == null ? null : current.createDiagnosticReport();
    }

    private boolean isSystemVpnStart(String action) {
        if (ACTION_CONNECT.equals(action)
                || ACTION_DISCONNECT.equals(action)
                || ACTION_DEBUG_COMMAND.equals(action)
                || VpnManager.ACTION_VPN_MANAGER_EVENT.equals(action)) {
            return false;
        }
        return action == null
                || SERVICE_INTERFACE.equals(action)
                || (Build.VERSION.SDK_INT >= 29 && isAlwaysOn());
    }

    private static VpnMode vpnManagerEventMode(Intent intent) {
        if (Build.VERSION.SDK_INT < 33
                || intent == null
                || !VpnManager.ACTION_VPN_MANAGER_EVENT.equals(intent.getAction())
                || !intent.hasCategory(VpnManager.CATEGORY_EVENT_ALWAYS_ON_STATE_CHANGED)) {
            return null;
        }
        VpnProfileState state =
                intent.getParcelableExtra(
                        VpnManager.EXTRA_VPN_PROFILE_STATE, VpnProfileState.class);
        if (state == null) {
            return VpnMode.manual();
        }
        return VpnMode.resolve(
                Build.VERSION.SDK_INT,
                false,
                state.isAlwaysOn(),
                state.isLockdownEnabled());
    }

    static boolean shouldRestartAfterProcessDeath(
            String action, boolean systemStart, boolean connectionRequested, boolean alwaysOn) {
        if (ACTION_DISCONNECT.equals(action)) {
            return false;
        }
        return ACTION_CONNECT.equals(action)
                || ACTION_SET_NETWORK_ENABLED.equals(action)
                || systemStart
                || connectionRequested
                || alwaysOn;
    }

    private void refreshVpnMode(boolean systemStart) {
        if (Build.VERSION.SDK_INT < 29 && systemStart) {
            legacySystemStartObserved = true;
        }
        boolean platformAlwaysOn = Build.VERSION.SDK_INT >= 29 && isAlwaysOn();
        boolean platformLockdown = Build.VERSION.SDK_INT >= 29 && isLockdownEnabled();
        VpnMode observed =
                VpnMode.resolve(
                        Build.VERSION.SDK_INT,
                        legacySystemStartObserved,
                        platformAlwaysOn,
                        platformLockdown);
        VpnMode previous = vpnMode;
        VpnMode updated =
                VpnMode.stabilize(
                        Build.VERSION.SDK_INT, previous, observed, desiredConnected);
        boolean observationGap = !updated.equals(observed);
        if (observationGap != vpnModeObservationGap) {
            recordDiagnosticEvent(
                    observationGap
                            ? "vpn_mode_observation_gap_started"
                            : "vpn_mode_observation_gap_ended");
            vpnModeObservationGap = observationGap;
        }
        applyVpnMode(updated);
    }

    private void applyVpnMode(VpnMode updated) {
        VpnMode previous = vpnMode;
        vpnMode = updated;
        if (!updated.equals(previous)) {
            if (updated.lockdown) {
                recordDiagnosticEvent("vpn_mode_lockdown");
            } else if (updated.alwaysOn) {
                recordDiagnosticEvent("vpn_mode_always_on");
            } else {
                recordDiagnosticEvent("vpn_mode_manual");
            }
        }
    }

    private String vpnModeDetail() {
        if (vpnMode.lockdown) {
            return getString(R.string.vpn_mode_lockdown);
        }
        if (vpnMode.alwaysOn) {
            return getString(R.string.vpn_mode_always_on);
        }
        return getString(R.string.vpn_mode_manual);
    }

    private boolean isDebuggable() {
        return (getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0;
    }

    private static String pairingProgress(PairRpc.OperationStatus status) {
        String phase = status.phase.replace('_', ' ');
        return ("inviter".equals(status.role) ? "Inviting: " : "Joining: ") + phase;
    }

    private static List<String> runtimeMetrics(JSONObject status) throws P2pVpnException {
        JSONArray encodedLines = status.optJSONArray("lines");
        if (encodedLines == null) {
            throw new P2pVpnException("Native runtime status does not contain metrics");
        }
        List<String> lines = new ArrayList<>(encodedLines.length());
        for (int index = 0; index < encodedLines.length(); index++) {
            Object line = encodedLines.opt(index);
            if (line instanceof String) {
                lines.add((String) line);
            }
        }
        return lines;
    }

    private static String normalizeHostname(String hostname) {
        String normalized = hostname == null ? "" : hostname.trim().toLowerCase(Locale.ROOT);
        return normalized.isEmpty() ? null : normalized;
    }

    private static String requiredDebugSetting(JSONObject value, String key, int maximumLength)
            throws JSONException, P2pVpnException {
        String result = value.getString(key).trim();
        if (result.isEmpty() || result.length() > maximumLength) {
            throw new P2pVpnException("Debug profile setting is invalid: " + key);
        }
        return result;
    }

    private static String optionalDebugSetting(JSONObject value, String key, int maximumLength)
            throws JSONException, P2pVpnException {
        if (!value.has(key) || value.isNull(key)) {
            return null;
        }
        return boundedOptionalDebugSetting(value.getString(key), key, maximumLength);
    }

    static String boundedOptionalDebugSetting(String value, String key, int maximumLength)
            throws P2pVpnException {
        if (value == null || value.length() > maximumLength) {
            throw new P2pVpnException("Debug profile setting is invalid: " + key);
        }
        String result = value.trim();
        if (result.isEmpty()) {
            throw new P2pVpnException("Debug profile setting is invalid: " + key);
        }
        for (int index = 0; index < result.length(); index++) {
            if (Character.isISOControl(result.charAt(index))) {
                throw new P2pVpnException("Debug profile setting is invalid: " + key);
            }
        }
        return result;
    }

    static void validateDebugE2ePaths(
            String listen, String externalEndpoint, String relayReservation)
            throws P2pVpnException {
        if ((listen == null) != (externalEndpoint == null)) {
            throw new P2pVpnException(
                    "Debug profile requires both owned QUIC packet endpoints");
        }
        if (listen != null && relayReservation != null) {
            throw new P2pVpnException(
                    "Debug profile cannot combine owned QUIC and relay-only paths");
        }
    }

    private static String failureMessage(Throwable error) {
        String message = error.getMessage();
        return message == null || message.trim().isEmpty()
                ? error.getClass().getSimpleName()
                : message;
    }

    private static String capitalize(String value) {
        if (value == null || value.isEmpty()) {
            return "Running";
        }
        return Character.toUpperCase(value.charAt(0)) + value.substring(1).replace('_', ' ');
    }

    private static long increment(long value) {
        return value == Long.MAX_VALUE ? value : value + 1;
    }

    private static void cancel(ScheduledFuture<?> future) {
        if (future != null) {
            future.cancel(false);
        }
    }

    public interface Listener {
        void onSnapshot(Snapshot snapshot);
    }

    public final class LocalBinder extends Binder {
        void addListener(Listener listener) {
            listeners.add(listener);
            Snapshot current = snapshot;
            mainHandler.post(() -> listener.onSnapshot(current));
        }

        void removeListener(Listener listener) {
            listeners.remove(listener);
        }

        void createProfile(String networkName) {
            worker.execute(() -> P2pVpnService.this.createUserProfile(networkName));
        }

        void selectNetwork(String networkId) {
            worker.execute(() -> P2pVpnService.this.selectNetwork(networkId));
        }

        void setNetworkEnabled(String networkId, boolean enabled) {
            worker.execute(
                    () -> P2pVpnService.this.setNetworkEnabled(networkId, enabled));
        }

        void renameNetwork(String networkId, String hostname) {
            worker.execute(() -> P2pVpnService.this.renameNetwork(networkId, hostname));
        }

        void removeNetwork(String networkId) {
            worker.execute(() -> P2pVpnService.this.removeNetwork(networkId));
        }

        void resetUnreadableProfile() {
            worker.execute(P2pVpnService.this::resetUnreadableProfile);
        }

        void openPairing() {
            worker.execute(P2pVpnService.this::openPairing);
        }

        void joinPairing(String code) {
            worker.execute(() -> P2pVpnService.this.joinPairing(code));
        }

        void cancelProfileJoin() {
            worker.execute(P2pVpnService.this::cancelProfileJoin);
        }

        void approvePairing(String hostname) {
            worker.execute(() -> P2pVpnService.this.approvePairing(hostname));
        }

        void rejectPairing() {
            worker.execute(P2pVpnService.this::rejectPairing);
        }

        String createDiagnosticReport() {
            return P2pVpnService.this.createDiagnosticReport();
        }
    }

    public static final class Snapshot {
        final boolean hasProfile;
        final boolean profileStored;
        final boolean profileUnreadable;
        final boolean connected;
        final boolean connectionRequested;
        final boolean busy;
        final boolean alwaysOn;
        final boolean lockdown;
        final String networkName;
        final String hostname;
        final String peerId;
        final List<String> addresses;
        final String selectedNetworkId;
        final List<NetworkSnapshot> networks;
        final RuntimeSummary runtimeSummary;
        final RuntimeDiagnostics runtimeDiagnostics;
        final long runtimeGeneration;
        final long runtimeNetworkChangeRequests;
        final long runtimeNetworkChangeFailures;
        final UnderlayTracker.Snapshot underlay;
        final String vpnModeDetail;
        final String connectionDetail;
        final String peerDetail;
        final boolean pairingActive;
        final boolean profileJoinActive;
        final String pairingDetail;
        final String pairingCode;
        final String candidatePeer;
        final String candidateFingerprint;
        final String candidateHostname;
        final String candidateVpnIp;

        private Snapshot(
                boolean hasProfile,
                boolean profileStored,
                boolean profileUnreadable,
                boolean connected,
                boolean connectionRequested,
                boolean busy,
                boolean alwaysOn,
                boolean lockdown,
                String networkName,
                String hostname,
                String peerId,
                List<String> addresses,
                String selectedNetworkId,
                List<NetworkSnapshot> networks,
                RuntimeSummary runtimeSummary,
                RuntimeDiagnostics runtimeDiagnostics,
                long runtimeGeneration,
                long runtimeNetworkChangeRequests,
                long runtimeNetworkChangeFailures,
                UnderlayTracker.Snapshot underlay,
                String vpnModeDetail,
                String connectionDetail,
                String peerDetail,
                boolean pairingActive,
                boolean profileJoinActive,
                String pairingDetail,
                String pairingCode,
                String candidatePeer,
                String candidateFingerprint,
                String candidateHostname,
                String candidateVpnIp) {
            this.hasProfile = hasProfile;
            this.profileStored = profileStored;
            this.profileUnreadable = profileUnreadable;
            this.connected = connected;
            this.connectionRequested = connectionRequested;
            this.busy = busy;
            this.alwaysOn = alwaysOn;
            this.lockdown = lockdown;
            this.networkName = networkName;
            this.hostname = hostname;
            this.peerId = peerId;
            this.addresses = Collections.unmodifiableList(new ArrayList<>(addresses));
            this.selectedNetworkId = selectedNetworkId;
            this.networks = Collections.unmodifiableList(new ArrayList<>(networks));
            this.runtimeSummary = runtimeSummary;
            this.runtimeDiagnostics = runtimeDiagnostics;
            this.runtimeGeneration = runtimeGeneration;
            this.runtimeNetworkChangeRequests = runtimeNetworkChangeRequests;
            this.runtimeNetworkChangeFailures = runtimeNetworkChangeFailures;
            this.underlay = underlay;
            this.vpnModeDetail = vpnModeDetail;
            this.connectionDetail = connectionDetail;
            this.peerDetail = peerDetail;
            this.pairingActive = pairingActive;
            this.profileJoinActive = profileJoinActive;
            this.pairingDetail = pairingDetail;
            this.pairingCode = pairingCode;
            this.candidatePeer = candidatePeer;
            this.candidateFingerprint = candidateFingerprint;
            this.candidateHostname = candidateHostname;
            this.candidateVpnIp = candidateVpnIp;
        }

        private static Snapshot initial() {
            return new Snapshot(
                    false,
                    false,
                    false,
                    false,
                    false,
                    true,
                    false,
                    false,
                    null,
                    null,
                    null,
                    Collections.emptyList(),
                    null,
                    Collections.emptyList(),
                    RuntimeSummary.empty(),
                    RuntimeDiagnostics.empty(),
                    0,
                    0,
                    0,
                    UnderlayTracker.Snapshot.empty(),
                    "Manual connection",
                    "Loading",
                    "Overlay peers: unavailable",
                    false,
                    false,
                    "No pairing operation",
                    null,
                    null,
                    null,
                    null,
                    null);
        }
    }

    public static final class NetworkSnapshot {
        final String id;
        final String name;
        final String hostname;
        final String peerId;
        final List<String> addresses;
        final boolean enabled;
        final boolean selected;
        final String phase;
        final String detail;
        final PeerSnapshot peers;

        private NetworkSnapshot(
                String id,
                String name,
                String hostname,
                String peerId,
                List<String> addresses,
                boolean enabled,
                boolean selected,
                String phase,
                String detail,
                PeerSnapshot peers) {
            this.id = id;
            this.name = name;
            this.hostname = hostname;
            this.peerId = peerId;
            this.addresses = Collections.unmodifiableList(new ArrayList<>(addresses));
            this.enabled = enabled;
            this.selected = selected;
            this.phase = phase;
            this.detail = detail;
            this.peers = peers;
        }
    }

    private static final class RuntimeFiles {
        final File directory;
        final File pairingState;
        final File membershipState;

        RuntimeFiles(File directory, File pairingState, File membershipState) {
            this.directory = directory;
            this.pairingState = pairingState;
            this.membershipState = membershipState;
        }
    }

    static final class NetworkRuntimeStatus {
        final String id;
        final String phase;
        final String detail;
        final List<String> metrics;
        final Optional<PeerSnapshot> peerSnapshot;

        private NetworkRuntimeStatus(
                String id,
                String phase,
                String detail,
                List<String> metrics,
                Optional<PeerSnapshot> peerSnapshot) {
            this.id = id;
            this.phase = phase;
            this.detail = detail;
            this.metrics = Collections.unmodifiableList(new ArrayList<>(metrics));
            this.peerSnapshot = peerSnapshot;
        }

        static NetworkRuntimeStatus from(JSONObject value) throws P2pVpnException {
            try {
                String id =
                        ProfileCollection.Entry.normalizeNetworkId(value.getString("id"));
                String phase = requireRuntimePhase(value.getString("phase"));
                String detail =
                        value.isNull("detail") ? "" : value.optString("detail", "");
                return new NetworkRuntimeStatus(
                        id,
                        phase,
                        detail,
                        runtimeMetrics(value),
                        PeerSnapshot.parseOptional(value, "peer_snapshot"));
            } catch (JSONException error) {
                throw new P2pVpnException("Native network runtime status is malformed", error);
            }
        }

        boolean isAvailable() {
            return "running".equals(phase);
        }
    }

    static final class RuntimeStatusSnapshot {
        final String phase;
        final String detail;
        final Map<String, NetworkRuntimeStatus> networks;
        final List<String> metrics;

        private RuntimeStatusSnapshot(
                String phase,
                String detail,
                Map<String, NetworkRuntimeStatus> networks,
                List<String> metrics) {
            this.phase = phase;
            this.detail = detail;
            this.networks = Collections.unmodifiableMap(new LinkedHashMap<>(networks));
            this.metrics = Collections.unmodifiableList(new ArrayList<>(metrics));
        }

        static RuntimeStatusSnapshot from(JSONObject value, List<String> expectedNetworkIds)
                throws P2pVpnException {
            if (expectedNetworkIds == null || expectedNetworkIds.isEmpty()) {
                throw new P2pVpnException("Native runtime has no expected networks");
            }
            try {
                String phase = requireRuntimePhase(value.getString("phase"));
                String detail =
                        value.isNull("detail") ? "" : value.optString("detail", "");
                JSONArray encodedNetworks = value.getJSONArray("networks");
                Map<String, NetworkRuntimeStatus> networks = new LinkedHashMap<>();
                for (int index = 0; index < encodedNetworks.length(); index++) {
                    NetworkRuntimeStatus network =
                            NetworkRuntimeStatus.from(encodedNetworks.getJSONObject(index));
                    if (networks.put(network.id, network) != null) {
                        throw new P2pVpnException(
                                "Native runtime status contains a duplicate network");
                    }
                }
                if (networks.size() != expectedNetworkIds.size()
                        || !networks.keySet().containsAll(expectedNetworkIds)) {
                    throw new P2pVpnException(
                            "Native runtime status does not match the enabled networks");
                }
                List<String> metrics = new ArrayList<>(runtimeMetrics(value));
                if (networks.size() > 1) {
                    for (String networkId : expectedNetworkIds) {
                        metrics.addAll(networks.get(networkId).metrics);
                    }
                }
                return new RuntimeStatusSnapshot(phase, detail, networks, metrics);
            } catch (JSONException error) {
                throw new P2pVpnException("Native runtime status is malformed", error);
            }
        }

        boolean requiresWholeRuntimeRestart() {
            return "failed".equals(phase) || "stopped".equals(phase);
        }

        String describeConnection() {
            if (networks.size() == 1) {
                if (!detail.isEmpty()) {
                    return capitalize(phase) + ": " + detail;
                }
                return capitalize(phase);
            }
            int running = 0;
            int starting = 0;
            int unavailable = 0;
            for (NetworkRuntimeStatus network : networks.values()) {
                if ("running".equals(network.phase)) {
                    running++;
                } else if ("starting".equals(network.phase)) {
                    starting++;
                } else {
                    unavailable++;
                }
            }
            if (unavailable > 0) {
                return "Connected: "
                        + running
                        + " running, "
                        + starting
                        + " starting, "
                        + unavailable
                        + " unavailable";
            }
            if (starting > 0) {
                return "Connecting: " + running + " running, " + starting + " starting";
            }
            return "Connected: " + running + " networks";
        }
    }

    private static String requireRuntimePhase(String phase) throws P2pVpnException {
        if ("starting".equals(phase)
                || "running".equals(phase)
                || "stopped".equals(phase)
                || "failed".equals(phase)) {
            return phase;
        }
        throw new P2pVpnException("Native runtime status contains an invalid phase");
    }

    private static final class ProfileJoinOperation {
        final String id;

        private ProfileJoinOperation(String id) {
            this.id = id;
        }
    }

    private static final class ProfileJoinResult {
        final AndroidProfile profile;
        final String error;

        private ProfileJoinResult(AndroidProfile profile, String error) {
            this.profile = profile;
            this.error = error;
        }

        static ProfileJoinResult success(AndroidProfile profile) {
            return new ProfileJoinResult(profile, null);
        }

        static ProfileJoinResult failure(String error) {
            return new ProfileJoinResult(null, error);
        }
    }

    static final class ActivePairing {
        private static final int LEGACY_VERSION = 1;
        private static final int VERSION = 2;

        enum Role {
            INVITER,
            JOINER
        }

        final String networkId;
        final String operationId;
        final Role role;
        final boolean needsMigration;
        String code;
        boolean started;
        String transcriptSha256;
        String remotePeer;
        PairRpc.Candidate candidate;

        private ActivePairing(
                String networkId,
                String operationId,
                Role role,
                String code,
                boolean needsMigration) {
            this.networkId = networkId;
            this.operationId = operationId;
            this.role = role;
            this.code = code;
            this.needsMigration = needsMigration;
        }

        static ActivePairing inviter(String networkId, String operationId)
                throws P2pVpnException {
            return new ActivePairing(
                    ProfileCollection.Entry.normalizeNetworkId(networkId),
                    operationId,
                    Role.INVITER,
                    null,
                    false);
        }

        static ActivePairing joiner(String networkId, String operationId, String code)
                throws P2pVpnException {
            return new ActivePairing(
                    ProfileCollection.Entry.normalizeNetworkId(networkId),
                    operationId,
                    Role.JOINER,
                    code,
                    false);
        }

        String displayCode() {
            return role == Role.INVITER ? code : null;
        }

        String toJson() throws P2pVpnException {
            try {
                JSONObject value = new JSONObject();
                value.put("version", VERSION);
                value.put("network_id", networkId);
                value.put("operation_id", operationId);
                value.put("role", role == Role.INVITER ? "inviter" : "joiner");
                value.put("code", code == null ? JSONObject.NULL : code);
                value.put("started", started);
                value.put(
                        "transcript_sha256",
                        transcriptSha256 == null ? JSONObject.NULL : transcriptSha256);
                value.put("remote_peer", remotePeer == null ? JSONObject.NULL : remotePeer);
                return value.toString();
            } catch (JSONException error) {
                throw new P2pVpnException("Failed to encode pairing recovery state", error);
            }
        }

        static ActivePairing fromJson(String encoded, String legacyNetworkId)
                throws P2pVpnException {
            try {
                JSONObject value = new JSONObject(encoded);
                int version = value.getInt("version");
                if (version != LEGACY_VERSION && version != VERSION) {
                    throw new P2pVpnException("Saved pairing operation has an unknown version");
                }
                boolean needsMigration = version == LEGACY_VERSION;
                String networkId =
                        ProfileCollection.Entry.normalizeNetworkId(
                                needsMigration
                                        ? legacyNetworkId
                                        : value.getString("network_id"));
                String operationId = value.getString("operation_id");
                String roleName = value.getString("role");
                Role role;
                if ("inviter".equals(roleName)) {
                    role = Role.INVITER;
                } else if ("joiner".equals(roleName)) {
                    role = Role.JOINER;
                } else {
                    throw new P2pVpnException("Saved pairing operation has an invalid role");
                }
                String code = nullableString(value, "code");
                boolean started = value.getBoolean("started");
                String transcript = nullableString(value, "transcript_sha256");
                String remotePeer = nullableString(value, "remote_peer");
                if (operationId.isEmpty() || operationId.length() > 128) {
                    throw new P2pVpnException("Saved pairing operation ID is invalid");
                }
                if (code != null && (code.isEmpty() || code.length() > 64)) {
                    throw new P2pVpnException("Saved pairing code is invalid");
                }
                if ((role == Role.JOINER && code == null)
                        || (role == Role.INVITER && started && code == null)) {
                    throw new P2pVpnException("Saved pairing operation is incomplete");
                }
                if (transcript != null && !transcript.matches("[0-9a-f]{64}")) {
                    throw new P2pVpnException("Saved pairing receipt is invalid");
                }
                if (transcript != null && (remotePeer == null || remotePeer.length() > 256)) {
                    throw new P2pVpnException("Saved pairing peer is invalid");
                }
                if ((transcript != null && !started)
                        || (transcript == null && remotePeer != null)) {
                    throw new P2pVpnException("Saved pairing recovery phase is invalid");
                }
                ActivePairing pairing =
                        new ActivePairing(
                                networkId, operationId, role, code, needsMigration);
                pairing.started = started;
                pairing.transcriptSha256 = transcript;
                pairing.remotePeer = remotePeer;
                return pairing;
            } catch (JSONException error) {
                throw new P2pVpnException("Saved pairing operation is malformed", error);
            }
        }

        private static String nullableString(JSONObject value, String key) {
            if (value.isNull(key)) {
                return null;
            }
            String result = value.optString(key, null);
            return result == null || result.isEmpty() ? null : result;
        }
    }
}
