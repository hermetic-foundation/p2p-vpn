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
import java.nio.file.Files;
import java.time.Instant;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Locale;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
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
    static final String ACTION_DEBUG_COMMAND = "org.hermeticfoundation.p2pvpn.debug.COMMAND";
    static final String EXTRA_DEBUG_COMMAND = "command";
    static final String EXTRA_DEBUG_VALUE = "value";
    static final int DEBUG_PACKET_QUIC_ENDPOINT_MAX_LENGTH = 512;
    static final int DEBUG_RELAY_RESERVATION_MAX_LENGTH = 1_024;

    private static final String NOTIFICATION_CHANNEL = "p2p-vpn-connection";
    private static final int NOTIFICATION_ID = 1;
    private static final long PAIRING_TIMEOUT_SECONDS = 600;
    private static final long PAIRING_POLL_MILLIS = 1_000;
    private static final long STATUS_POLL_MILLIS = 2_000;
    private static final long NETWORK_RECONNECT_DELAY_MILLIS = 1_500;
    private static final long MAX_RECONNECT_DELAY_MILLIS = 30_000;
    private static final long UNDERLAY_RESTART_DELAY_MILLIS = 500;
    private static volatile P2pVpnService debugInstance;

    private final LocalBinder localBinder = new LocalBinder();
    private final Handler mainHandler = new Handler(Looper.getMainLooper());
    private final Set<Listener> listeners =
            Collections.newSetFromMap(new ConcurrentHashMap<Listener, Boolean>());
    private final UnderlayTracker underlayTracker = new UnderlayTracker();
    private final DiagnosticEventBuffer diagnosticEvents = new DiagnosticEventBuffer();

    private ScheduledThreadPoolExecutor worker;
    private ProfileStore profileStore;
    private File runtimeDirectory;
    private File pairingStateFile;
    private File membershipStateFile;
    private boolean runtimeStorageReady;
    private ConnectivityManager connectivityManager;
    private ConnectivityManager.NetworkCallback networkCallback;
    private WifiManager.MulticastLock multicastLock;
    private ScheduledFuture<?> reconnectFuture;
    private ScheduledFuture<?> underlayRecoveryFuture;
    private ScheduledFuture<?> statusFuture;
    private ScheduledFuture<?> pairingFuture;

    private AndroidProfile profile;
    private boolean profilePresent;
    private boolean profileUnreadable;
    private ActivePairing activePairing;
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
        profileStore = new ProfileStore(this);
        prepareRuntimeDirectory();
        createNotificationChannel();
        registerNetworkCallback();
        worker.execute(this::loadProfileMetadata);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? null : intent.getAction();
        boolean systemStart = isSystemVpnStart(action);
        refreshVpnMode(systemStart);
        if (ACTION_CONNECT.equals(action)) {
            startForeground(
                    NOTIFICATION_ID,
                    notification(getString(R.string.notification_connecting)));
            worker.execute(() -> connectRequested(false));
        } else if (ACTION_DISCONNECT.equals(action)) {
            worker.execute(() -> disconnectRequested(false));
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
            worker.execute(() -> connectRequested(true));
        }
        return START_NOT_STICKY;
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
        mainHandler.post(
                () -> {
                    stopForeground(STOP_FOREGROUND_REMOVE);
                    stopSelf();
                });
    }

    private void startConnection(String initialDetail) {
        if (!desiredConnected || operationInProgress) {
            return;
        }
        refreshVpnMode(false);
        if (!vpnMode.permitsOverlayConnection()) {
            connectionDetail = getString(R.string.lockdown_unsupported);
            recordDiagnosticEvent("lockdown_connection_blocked");
            updateForegroundNotification();
            publishSnapshot();
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
        if (connectivityManager != null
                && underlayTracker.snapshot().availableNetworks == 0) {
            connectionDetail = getString(R.string.waiting_for_underlay);
            recordDiagnosticEvent("connection_waiting_for_underlay");
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
            acquireMulticastLock();
            Builder builder = new Builder();
            builder.setSession(profile.networkName);
            builder.setMtu(profile.mtu);
            builder.setBlocking(true);
            try {
                // Every socket created by this process is VPN underlay traffic.
                builder.addDisallowedApplication(getPackageName());
            } catch (PackageManager.NameNotFoundException error) {
                throw new P2pVpnException("Failed to isolate VPN transport sockets", error);
            }
            for (AndroidProfile.Cidr address : profile.addresses) {
                builder.addAddress(address.inetAddress, address.prefixLength);
            }
            for (AndroidProfile.Cidr route : profile.routes) {
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
            NativeResponse.objectValue(
                    NativeBridge.nativeStart(
                            profile.configJson,
                            tunFd,
                            pairingStateFile.getAbsolutePath(),
                            membershipStateFile.getAbsolutePath()));
            if (runtimeGeneration < Long.MAX_VALUE) {
                runtimeGeneration++;
            }
            connected = true;
            recordDiagnosticEvent("runtime_started");
            reconnectAttempts = 0;
            cancel(reconnectFuture);
            reconnectFuture = null;
            connectionDetail = "Connected";
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
        cancel(statusFuture);
        statusFuture = null;
        try {
            NativeResponse.objectValue(NativeBridge.nativeStop());
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            connectionDetail = failureMessage(error);
        }
        connected = false;
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
            cancel(underlayRecoveryFuture);
            underlayRecoveryFuture =
                    worker.schedule(
                            this::restartAfterUnderlayChange,
                            UNDERLAY_RESTART_DELAY_MILLIS,
                            TimeUnit.MILLISECONDS);
        } else if (change == UnderlayTracker.Change.INITIAL
                && desiredConnected
                && !connected
                && !operationInProgress) {
            startConnection("Connecting on the available network");
        }
        publishSnapshot();
    }

    private void restartAfterUnderlayChange() {
        underlayRecoveryFuture = null;
        if (!desiredConnected) {
            return;
        }
        if (operationInProgress) {
            underlayRecoveryFuture =
                    worker.schedule(
                            this::restartAfterUnderlayChange,
                            UNDERLAY_RESTART_DELAY_MILLIS,
                            TimeUnit.MILLISECONDS);
            return;
        }
        runtimeNetworkChangeRequests = increment(runtimeNetworkChangeRequests);
        recordDiagnosticEvent("underlay_recovery_requested");
        long requestSequence = runtimeNetworkChangeRequests;
        Log.i(LOG_TAG, "event=underlay_runtime_restart_requested sequence=" + requestSequence);
        if (connectivityManager != null
                && underlayTracker.snapshot().availableNetworks == 0) {
            operationInProgress = true;
            stopNativeRuntime();
            operationInProgress = false;
            connectionDetail = getString(R.string.waiting_for_underlay);
            updateForegroundNotification();
            publishSnapshot();
            return;
        }
        reconnectDetail = "Recreating transport sockets after network change";
        reconnectAfterNetworkChange();
        if (connected) {
            recordDiagnosticEvent("underlay_recovery_completed");
            Log.i(
                    LOG_TAG,
                    "event=underlay_runtime_restart_completed sequence=" + requestSequence);
        } else {
            runtimeNetworkChangeFailures = increment(runtimeNetworkChangeFailures);
            recordDiagnosticEvent("underlay_recovery_failed");
            Log.w(LOG_TAG, "event=underlay_runtime_restart_failed sequence=" + requestSequence);
        }
    }

    private void scheduleStatusPoll() {
        cancel(statusFuture);
        statusFuture =
                worker.schedule(this::pollNativeStatus, STATUS_POLL_MILLIS, TimeUnit.MILLISECONDS);
    }

    private void pollNativeStatus() {
        statusFuture = null;
        if (!connected) {
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
            String phase = status.optString("phase", "running");
            String detail = status.isNull("detail") ? "" : status.optString("detail", "");
            if ("failed".equals(phase) || "stopped".equals(phase)) {
                runtimeFailed = true;
                failure = detail.isEmpty() ? "Native runtime stopped" : detail;
            } else if (!detail.isEmpty()) {
                connectionDetail = capitalize(phase) + ": " + detail;
            } else {
                connectionDetail = capitalize(phase);
            }
            List<String> metrics = runtimeMetrics(status);
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
                activePairing = ActivePairing.fromJson(profileStore.loadPairing());
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
        String configJson = profileStore.load();
        profile =
                AndroidProfile.fromNative(
                        NativeResponse.objectValue(NativeBridge.nativeInspectProfile(configJson)));
    }

    private void createProfile(String networkName) {
        createProfile(networkName, null, null, null, null, null, null);
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
            validateDebugE2ePaths(
                    packetQuicListen, packetQuicExternalEndpoint, relayReservation);
            createProfile(
                    requiredDebugSetting(settings, "network", 128),
                    requiredDebugSetting(settings, "bootstrap_peer_id", 256),
                    requiredDebugSetting(settings, "bootstrap_address", 1_024),
                    requiredDebugSetting(settings, "kademlia_protocol", 128),
                    packetQuicListen,
                    packetQuicExternalEndpoint,
                    relayReservation);
        } catch (P2pVpnException | JSONException | RuntimeException error) {
            connectionDetail = failureMessage(error);
            publishSnapshot();
        }
    }

    private void createProfile(
            String networkName,
            String bootstrapPeerId,
            String bootstrapAddress,
            String kademliaProtocol,
            String packetQuicListen,
            String packetQuicExternalEndpoint,
            String relayReservation) {
        if (operationInProgress) {
            return;
        }
        operationInProgress = true;
        connectionDetail = "Creating profile";
        publishSnapshot();
        try {
            if (profileStore.exists()) {
                throw new P2pVpnException("This device already has a p2p-vpn profile");
            }
            AndroidProfile created =
                    AndroidProfile.fromNative(
                            NativeResponse.objectValue(
                                    bootstrapPeerId == null
                                            ? NativeBridge.nativeCreateProfile(networkName.trim())
                                            : NativeBridge.nativeCreateE2eProfile(
                                                    networkName.trim(),
                                                    bootstrapPeerId,
                                                    bootstrapAddress,
                                                    kademliaProtocol,
                                                    packetQuicListen,
                                                    packetQuicExternalEndpoint,
                                                    relayReservation)));
            profileStore.save(created.configJson);
            profile = created;
            profilePresent = true;
            profileUnreadable = false;
            recordDiagnosticEvent("profile_created");
            connectionDetail = "Profile ready";
        } catch (P2pVpnException | RuntimeException | LinkageError error) {
            connectionDetail = failureMessage(error);
        } finally {
            operationInProgress = false;
            publishSnapshot();
            if (profile != null && desiredConnected && !connected) {
                startConnection("Connecting with the new profile");
            }
        }
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
        Files.deleteIfExists(pairingStateFile.toPath());
        Files.deleteIfExists(membershipStateFile.toPath());
        Files.deleteIfExists(new File(runtimeDirectory, "membership.key").toPath());
    }

    private void openPairing() {
        if (!requireConnectedForPairing()) {
            return;
        }
        try {
            activePairing = ActivePairing.inviter(PairingOperationId.generate());
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
            activePairing = ActivePairing.joiner(PairingOperationId.generate(), normalized);
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
                                        PairRpc.open(
                                                activePairing.operationId,
                                                PAIRING_TIMEOUT_SECONDS))
                                : PairRpc.call(
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
                            PairRpc.call(PairRpc.status(activePairing.operationId)));
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
                    PairRpc.call(PairRpc.artifacts(activePairing.operationId));
            if (!"artifacts".equals(result.kind)) {
                throw new P2pVpnException("Pairing RPC did not return enrollment artifacts");
            }
            JSONObject artifacts = result.value;
            JSONObject receipt = artifacts.getJSONObject("receipt");
            String transcript = receipt.getString("transcript_sha256");
            String remotePeer = receipt.getString("remote_peer");

            String currentConfig = profileStore.load();
            AndroidProfile updated =
                    AndroidProfile.fromNative(
                            NativeResponse.objectValue(
                                    NativeBridge.nativeApplyPairingArtifacts(
                                            currentConfig,
                                            artifacts.toString(),
                                            runtimeDirectory.getAbsolutePath())));

            // Durably save the new identity bindings before compacting native enrollment state.
            profileStore.save(updated.configJson);
            profile = updated;
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
            PairRpc.call(PairRpc.cancel(activePairing.operationId));
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
        if (!runtimeDirectory.isDirectory() && !runtimeDirectory.mkdirs()) {
            connectionDetail = "Failed to create private runtime storage";
            return;
        }
        runtimeDirectory.setReadable(false, false);
        runtimeDirectory.setWritable(false, false);
        runtimeDirectory.setExecutable(false, false);
        runtimeDirectory.setReadable(true, true);
        runtimeDirectory.setWritable(true, true);
        runtimeDirectory.setExecutable(true, true);
        pairingStateFile = new File(runtimeDirectory, "pairing-state.json");
        membershipStateFile = new File(runtimeDirectory, "membership-state.json");
        try {
            Files.deleteIfExists(new File(runtimeDirectory, "membership.key").toPath());
            runtimeStorageReady = true;
        } catch (IOException error) {
            connectionDetail = "Failed to remove a stale pairing secret";
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
                connected && profile != null
                        ? getString(R.string.notification_connected, profile.networkName)
                        : connectionDetail;
        manager.notify(NOTIFICATION_ID, notification(message));
    }

    private void publishSnapshot() {
        refreshVpnMode(false);
        PairRpc.Candidate candidate = activePairing == null ? null : activePairing.candidate;
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
                        latestRuntimeSummary,
                        latestRuntimeDiagnostics,
                        runtimeGeneration,
                        runtimeNetworkChangeRequests,
                        runtimeNetworkChangeFailures,
                        underlayTracker.snapshot(),
                        vpnModeDetail(),
                        connectionDetail,
                        peerDetail,
                        activePairing != null,
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
                || ACTION_DEBUG_COMMAND.equals(action)) {
            return false;
        }
        return action == null || (Build.VERSION.SDK_INT >= 29 && isAlwaysOn());
    }

    private void refreshVpnMode(boolean systemStart) {
        if (Build.VERSION.SDK_INT < 29 && systemStart) {
            legacySystemStartObserved = true;
        }
        boolean platformAlwaysOn = Build.VERSION.SDK_INT >= 29 && isAlwaysOn();
        boolean platformLockdown = Build.VERSION.SDK_INT >= 29 && isLockdownEnabled();
        VpnMode updated =
                VpnMode.resolve(
                        Build.VERSION.SDK_INT,
                        legacySystemStartObserved,
                        platformAlwaysOn,
                        platformLockdown);
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
            worker.execute(() -> P2pVpnService.this.createProfile(networkName));
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
                    "No pairing operation",
                    null,
                    null,
                    null,
                    null,
                    null);
        }
    }

    private static final class ActivePairing {
        private static final int VERSION = 1;

        enum Role {
            INVITER,
            JOINER
        }

        final String operationId;
        final Role role;
        String code;
        boolean started;
        String transcriptSha256;
        String remotePeer;
        PairRpc.Candidate candidate;

        private ActivePairing(String operationId, Role role, String code) {
            this.operationId = operationId;
            this.role = role;
            this.code = code;
        }

        static ActivePairing inviter(String operationId) {
            return new ActivePairing(operationId, Role.INVITER, null);
        }

        static ActivePairing joiner(String operationId, String code) {
            return new ActivePairing(operationId, Role.JOINER, code);
        }

        String displayCode() {
            return role == Role.INVITER ? code : null;
        }

        String toJson() throws P2pVpnException {
            try {
                JSONObject value = new JSONObject();
                value.put("version", VERSION);
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

        static ActivePairing fromJson(String encoded) throws P2pVpnException {
            try {
                JSONObject value = new JSONObject(encoded);
                if (value.getInt("version") != VERSION) {
                    throw new P2pVpnException("Saved pairing operation has an unknown version");
                }
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
                ActivePairing pairing = new ActivePairing(operationId, role, code);
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
