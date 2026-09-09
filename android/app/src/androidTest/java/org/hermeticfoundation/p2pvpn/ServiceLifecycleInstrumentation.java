package org.hermeticfoundation.p2pvpn;

import android.app.Activity;
import android.app.ActivityManager;
import android.app.Instrumentation;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
import android.net.VpnService;
import android.net.VpnManager;
import android.os.Build;
import android.os.Bundle;
import android.os.IBinder;
import android.os.ParcelFileDescriptor;
import android.os.SystemClock;
import android.util.Log;
import java.lang.reflect.Field;
import java.lang.reflect.Constructor;
import java.lang.reflect.Method;
import java.io.IOException;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Callable;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;
import java.util.function.BooleanSupplier;

/** Runs only in a disposable emulator; no fault-injection hooks enter the app APK. */
public final class ServiceLifecycleInstrumentation extends Instrumentation {
    private Bundle arguments;
    private ServiceConnection connection;
    private ServiceRuntimeWorker.Scope currentScope;

    @Override
    public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        this.arguments = arguments;
        start();
    }

    @Override
    public void onStart() {
        Bundle result = new Bundle();
        Bundle status = new Bundle();
        status.putString("id", "InstrumentationTestRunner");
        status.putString("class", getClass().getName());
        status.putString("test", "occupiedWorkerServiceReplacement");
        status.putInt("numtests", 1);
        status.putInt("current", 1);
        sendStatus(1, status);
        try {
            require("true".equals(arguments.getString("isolated_emulator")), "explicit emulator opt-in required");
            require("ranchu".equals(Build.HARDWARE) || "goldfish".equals(Build.HARDWARE), "emulator required");
            require(!new ProfileStore(getTargetContext()).exists(), "test requires an empty profile store");
            exerciseReplacement();
            result.putString("stream", "OK: occupied-worker service replacement and native cleanup passed\n");
            result.putBoolean("passed", true);
            sendStatus(0, status);
            finish(Activity.RESULT_OK, result);
        } catch (Throwable error) {
            result.putString("stream", "FAIL: " + error + "\n");
            result.putBoolean("passed", false);
            status.putString("stack", Log.getStackTraceString(error));
            sendStatus(-2, status);
            finish(Activity.RESULT_CANCELED, result);
        }
    }

    private void exerciseReplacement() throws Exception {
        CountDownLatch entered = new CountDownLatch(1);
        CountDownLatch release = new CountDownLatch(1);
        ServiceRuntimeWorker.Scope old = null;
        try {
            bind();
            Context context = getTargetContext();
            onMain(() -> context.startService(serviceIntent()
                    .setAction(P2pVpnService.ACTION_DEBUG_COMMAND)
                    .putExtra(P2pVpnService.EXTRA_DEBUG_COMMAND, "create-profile")
                    .putExtra(P2pVpnService.EXTRA_DEBUG_VALUE, "lifecycle-test")));
            await(() -> {
                P2pVpnService.Snapshot snapshot = P2pVpnService.debugSnapshot();
                return snapshot != null && snapshot.hasProfile && !snapshot.busy;
            }, 30, "profile creation");
            if ("true".equals(arguments.getString("deferred_join"))) {
                exerciseDeferredJoin();
            }
            if ("true".equals(arguments.getString("cancelled_join"))) {
                exerciseCancelledJoin();
            }
            if ("true".equals(arguments.getString("superseded_stop"))) {
                exerciseSupersededStop();
            }
            String peer = P2pVpnService.debugSnapshot().peerId;
            connect();
            awaitNativeRunning();
            if ("true".equals(arguments.getString("health_poll"))) {
                exerciseHealthPoll();
            }

            old = currentScope;
            ScheduledFuture<?> occupied = old.schedule(() -> {
                entered.countDown();
                try {
                    require(release.await(30, TimeUnit.SECONDS), "worker was not released");
                } catch (InterruptedException error) {
                    Thread.currentThread().interrupt();
                    throw new AssertionError("native-like work was interrupted", error);
                }
            }, 0, TimeUnit.SECONDS);
            require(entered.await(5, TimeUnit.SECONDS), "worker did not become occupied");
            AtomicInteger stale = new AtomicInteger();
            ScheduledFuture<?> queued = old.schedule(stale::incrementAndGet, 0, TimeUnit.SECONDS);
            revokeVpn();
            long started = SystemClock.elapsedRealtime();
            unbindAndStop();
            ServiceRuntimeWorker.Scope retired = old;
            await(retired::isClosed, 2, "service destruction without waiting for worker");
            require(SystemClock.elapsedRealtime() - started < 2000, "main-thread teardown waited for worker");
            require(queued.isCancelled(), "retired pending work was not cancelled");
            require(!old.cleanupCompletion().isDone(), "cleanup ran concurrently with occupied worker");

            bind();
            require(currentScope != old, "Android did not create a replacement service");
            ScheduledFuture<?> beforeStart = currentScope.schedule(() -> {
                require("stopped".equals(nativePhase()), "replacement work ran before native cleanup");
            }, 0, TimeUnit.SECONDS);
            require(!beforeStart.isDone(), "replacement escaped occupied process dispatcher");
            old.execute(stale::incrementAndGet);
            old.close();
            release.countDown();
            occupied.get(5, TimeUnit.SECONDS);
            old.cleanupCompletion().get(10, TimeUnit.SECONDS);
            beforeStart.get(10, TimeUnit.SECONDS);
            require(stale.get() == 0, "retired service work executed");
            connect();
            awaitNativeRunning();
            require(peer.equals(P2pVpnService.debugSnapshot().peerId), "replacement changed profile identity");
        } finally {
            release.countDown();
            revokeVpn();
            unbindAndStop();
            if (old != null) {
                old.close();
                old.cleanupCompletion().get(10, TimeUnit.SECONDS);
            }
            if (currentScope != null) {
                await(currentScope::isClosed, 5, "final service destruction");
                currentScope.cleanupCompletion().get(10, TimeUnit.SECONDS);
                require("stopped".equals(nativePhase()), "final native runtime still running");
            }
        }
    }

    private void bind() throws Exception {
        CountDownLatch bound = new CountDownLatch(1);
        ServiceConnection binding = new ServiceConnection() {
            @Override
            public void onServiceConnected(ComponentName name, IBinder binder) { bound.countDown(); }
            @Override
            public void onServiceDisconnected(ComponentName name) {}
        };
        onMain(() -> {
            require(getTargetContext().bindService(serviceIntent(), binding, Context.BIND_AUTO_CREATE), "bind failed");
            connection = binding;
        });
        require(bound.await(5, TimeUnit.SECONDS), "service binding timed out");
        Field instance = P2pVpnService.class.getDeclaredField("debugInstance");
        instance.setAccessible(true);
        Field worker = P2pVpnService.class.getDeclaredField("worker");
        worker.setAccessible(true);
        currentScope = (ServiceRuntimeWorker.Scope) worker.get(instance.get(null));
    }

    private void exerciseHealthPoll() throws Exception {
        require(Build.VERSION.SDK_INT >= 33, "VPN manager events require API 33");
        P2pVpnService service = (P2pVpnService) serviceField(null, "debugInstance");
        await(() -> onWorker(() -> Boolean.TRUE.equals(serviceField(service, "connected"))
                && Boolean.FALSE.equals(serviceField(service, "operationInProgress"))
                && serviceField(service, "underlayRecoveryFuture") == null), 15, "settled service");
        Object generation = onWorker(() -> serviceField(service, "runtimeGeneration"));
        Object networks = onWorker(() -> serviceField(service, "activeNetworkIds"));
        String peer = P2pVpnService.debugSnapshot().peerId;
        Method arm = P2pVpnService.class.getDeclaredMethod("scheduleStatusPoll");
        arm.setAccessible(true);
        for (int i = 0; i < 3; i++) {
            ScheduledFuture<?> old = onWorker(() -> {
                arm.invoke(service);
                return (ScheduledFuture<?>) serviceField(service, "statusFuture");
            });
            onMain(() -> service.onStartCommand(new Intent(VpnManager.ACTION_VPN_MANAGER_EVENT)
                    .addCategory(VpnManager.CATEGORY_EVENT_ALWAYS_ON_STATE_CHANGED), 0, 0));
            onWorker(() -> {
                ScheduledFuture<?> next = (ScheduledFuture<?>) serviceField(service, "statusFuture");
                require(old.isCancelled(), "manager event did not retire its old poll");
                require(next != null && next != old && !next.isDone(), "manager event lost health polling");
                require(generation.equals(serviceField(service, "runtimeGeneration")), "manager event restarted runtime");
                return null;
            });
        }
        // No further callbacks or explicit poll scheduling may sustain this chain.
        for (int i = 0; i < 3; i++) {
            ScheduledFuture<?> poll = onWorker(() -> (ScheduledFuture<?>) serviceField(service, "statusFuture"));
            require(poll != null, "missing recurring poll");
            poll.get(5, TimeUnit.SECONDS);
            onWorker(() -> {
                ScheduledFuture<?> next = (ScheduledFuture<?>) serviceField(service, "statusFuture");
                require(!poll.isCancelled() && next != null && next != poll && !next.isDone(), "poll did not recur");
                require(generation.equals(serviceField(service, "runtimeGeneration")), "healthy polling restarted runtime");
                return null;
            });
        }
        DiagnosticEventBuffer events = (DiagnosticEventBuffer) serviceField(service, "diagnosticEvents");
        long previousSequence = events.snapshot().entries.stream().mapToLong(entry -> entry.sequence).max().orElse(0);
        onWorker(() -> {
            NativeResponse.objectValue(NativeBridge.nativeStop());
            require("stopped".equals(nativePhase()), "fault injection did not stop native runtime");
            require(Boolean.TRUE.equals(serviceField(service, "connected")), "fault changed Java connection state");
            require(Boolean.TRUE.equals(serviceField(service, "desiredConnected")), "connection intent was lost");
            require(serviceField(service, "statusFuture") != null, "fault cancelled health polling");
            return null;
        });
        await(() -> onWorker(() -> {
            if (Boolean.TRUE.equals(serviceField(service, "connected"))) { return false; }
            require(Boolean.TRUE.equals(serviceField(service, "desiredConnected")), "health failure lost intent");
            require(generation.equals(serviceField(service, "runtimeGeneration")), "unexpected early restart");
            require(Integer.valueOf(1).equals(serviceField(service, "reconnectAttempts")), "expected one recovery attempt");
            require("Recovering native runtime".equals(serviceField(service, "reconnectDetail")), "wrong recovery trigger");
            ScheduledFuture<?> retry = (ScheduledFuture<?>) serviceField(service, "reconnectFuture");
            require(retry != null && !retry.isDone(), "health failure did not schedule recovery");
            require(serviceField(service, "statusFuture") == null, "failed runtime kept polling");
            return true;
        }), 5, "health-poll failure detection");
        awaitNativeRunning();
        await(() -> onWorker(() -> Boolean.TRUE.equals(serviceField(service, "connected"))
                && Boolean.FALSE.equals(serviceField(service, "operationInProgress"))), 10, "service recovery");
        onWorker(() -> {
            require(Long.valueOf(((Long) generation) + 1).equals(serviceField(service, "runtimeGeneration")), "recovery generation mismatch");
            require(networks.equals(serviceField(service, "activeNetworkIds")), "recovery changed enabled networks");
            require(peer.equals(P2pVpnService.debugSnapshot().peerId), "recovery changed peer identity");
            require(serviceField(service, "reconnectFuture") == null, "recovery retained retry timer");
            require(Integer.valueOf(0).equals(serviceField(service, "reconnectAttempts")), "recovery retained backoff");
            ScheduledFuture<?> poll = (ScheduledFuture<?>) serviceField(service, "statusFuture");
            require(poll != null && !poll.isDone(), "recovery did not restore health polling");
            return null;
        });
        long failedSequence = events.snapshot().entries.stream()
                .filter(entry -> entry.sequence > previousSequence && "runtime_health_failed".equals(entry.name))
                .mapToLong(entry -> entry.sequence).findFirst().orElse(0);
        require(failedSequence > previousSequence, "missing native health failure evidence");
        require(events.snapshot().entries.stream().anyMatch(entry -> entry.sequence > failedSequence
                && "runtime_stopped".equals(entry.name)), "missing health-triggered stop evidence");
        Bundle status = new Bundle();
        status.putString("health_poll", "passed");
        sendStatus(0, status);
    }

    private static Object serviceField(P2pVpnService service, String name) {
        try {
            Field field = P2pVpnService.class.getDeclaredField(name);
            field.setAccessible(true);
            return field.get(service);
        } catch (ReflectiveOperationException error) {
            throw new AssertionError("service field unavailable: " + name, error);
        }
    }

    private <T> T onWorker(Callable<T> action) {
        AtomicReference<T> result = new AtomicReference<>();
        try {
            currentScope.schedule(() -> {
                try { result.set(action.call()); }
                catch (Exception error) { throw new AssertionError("worker action failed", error); }
            }, 0, TimeUnit.MILLISECONDS).get(5, TimeUnit.SECONDS);
            return result.get();
        } catch (Exception error) {
            throw new AssertionError("worker action did not complete", error);
        }
    }

    private void exerciseDeferredJoin() throws Exception {
        setVpnConsent(true);
        onMain(() -> require(VpnService.prepare(getTargetContext()) == null,
                "emulator VPN consent is required"));
        Field instance = P2pVpnService.class.getDeclaredField("debugInstance");
        instance.setAccessible(true);
        P2pVpnService service = (P2pVpnService) instance.get(null);
        Class<?> operationClass = Class.forName(P2pVpnService.class.getName() + "$ProfileJoinOperation");
        Class<?> resultClass = Class.forName(P2pVpnService.class.getName() + "$ProfileJoinResult");
        Constructor<?> operationConstructor = operationClass.getDeclaredConstructor(String.class);
        operationConstructor.setAccessible(true);
        Method failure = resultClass.getDeclaredMethod("failure", String.class);
        Method success = resultClass.getDeclaredMethod("success", AndroidProfile.class);
        Method complete = P2pVpnService.class.getDeclaredMethod("completeProfileJoin", String.class, resultClass);
        Method request = P2pVpnService.class.getDeclaredMethod("connectRequested", boolean.class);
        Method disconnect = P2pVpnService.class.getDeclaredMethod("disconnectRequested", boolean.class);
        for (Method method : new Method[] {failure, success, complete, request, disconnect}) {
            method.setAccessible(true);
        }
        Field operation = P2pVpnService.class.getDeclaredField("profileJoinOperation");
        Field busy = P2pVpnService.class.getDeclaredField("operationInProgress");
        Field desired = P2pVpnService.class.getDeclaredField("desiredConnected");
        Field collection = P2pVpnService.class.getDeclaredField("profileCollection");
        for (Field field : new Field[] {operation, busy, desired, collection}) {
            field.setAccessible(true);
        }
        // Hold the asynchronous result at the worker boundary; no public pairing is needed.
        for (int variant = 0; variant < 3; variant++) {
            int current = variant;
            currentScope.schedule(() -> {
                try {
                    String id = PairingOperationId.generate();
                    operation.set(service, operationConstructor.newInstance(id));
                    busy.setBoolean(service, true);
                    request.invoke(service, false);
                    require(desired.getBoolean(service), "busy join lost connect intent");
                    require(!P2pVpnService.debugSnapshot().connected, "connection started before join completion");
                    if (current == 1) {
                        desired.setBoolean(service, false);
                    }
                    ProfileCollection before = (ProfileCollection) collection.get(service);
                    Object result = current == 2
                            ? success.invoke(null, AndroidProfile.fromNative(NativeResponse.objectValue(
                                    NativeBridge.nativeCreateProfile("joined-lifecycle", "test-phone"))))
                            : failure.invoke(null, "Injected join failure");
                    complete.invoke(service, id, result);
                    require(!busy.getBoolean(service), "join retained operation ownership");
                    require(operation.get(service) == null, "join retained completion ownership");
                    if (current == 1) {
                        require(!P2pVpnService.debugSnapshot().connected, "withdrawn intent restarted VPN");
                    } else {
                        require(P2pVpnService.debugSnapshot().connected,
                                "join completion did not resume requested connection: "
                                        + P2pVpnService.debugSnapshot().connectionDetail);
                    }
                    if (current == 2) {
                        ProfileCollection saved = (ProfileCollection) collection.get(service);
                        require(saved.networks.size() == 2, "successful join did not persist second network");
                        int enabled = 0;
                        for (ProfileCollection.Entry network : saved.networks) {
                            if (network.enabled) enabled++;
                        }
                        require(enabled == 1, "join changed enabled network set");
                        for (ProfileCollection.Entry original : before.networks) {
                            require(saved.find(original.id).enabled == original.enabled,
                                    "join changed existing network activation");
                        }
                        for (ProfileCollection.Entry network : saved.networks) {
                            if (before.find(network.id) == null) {
                                require(!network.enabled, "joined network was enabled implicitly");
                            }
                        }
                    }
                    disconnect.invoke(service, true);
                } catch (ReflectiveOperationException | P2pVpnException error) {
                    throw new AssertionError(error);
                }
            }, 0, TimeUnit.SECONDS).get(30, TimeUnit.SECONDS);
        }
        sendStatus(2, message("deferred_join", "passed: failed join, withdrawn intent, successful disabled join"));
    }

    private void exerciseCancelledJoin() throws Exception {
        Field instance = P2pVpnService.class.getDeclaredField("debugInstance");
        instance.setAccessible(true);
        P2pVpnService service = (P2pVpnService) instance.get(null);
        Class<?> operationClass = Class.forName(P2pVpnService.class.getName() + "$ProfileJoinOperation");
        Class<?> resultClass = Class.forName(P2pVpnService.class.getName() + "$ProfileJoinResult");
        Constructor<?> constructor = operationClass.getDeclaredConstructor(String.class);
        constructor.setAccessible(true);
        Method success = resultClass.getDeclaredMethod("success", AndroidProfile.class);
        Method complete = P2pVpnService.class.getDeclaredMethod("completeProfileJoin", String.class, resultClass);
        Method cancel = P2pVpnService.class.getDeclaredMethod("cancelProfileJoin");
        for (Method method : new Method[] {success, complete, cancel}) {
            method.setAccessible(true);
        }
        Field operation = P2pVpnService.class.getDeclaredField("profileJoinOperation");
        Field busy = P2pVpnService.class.getDeclaredField("operationInProgress");
        for (Field field : new Field[] {operation, busy}) {
            field.setAccessible(true);
        }
        currentScope.schedule(() -> {
            try {
                ProfileStore store = new ProfileStore(getTargetContext());
                String before = store.load();
                String cancelledId = PairingOperationId.generate();
                Object cancelled = constructor.newInstance(cancelledId);
                Object result = success.invoke(null, AndroidProfile.fromNative(NativeResponse.objectValue(
                        NativeBridge.nativeCreateProfile("cancelled-lifecycle", "test-phone"))));
                operation.set(service, cancelled);
                busy.setBoolean(service, true);
                // Native has returned; cancellation wins admission on the service worker.
                cancel.invoke(service);
                cancel.invoke(service);
                complete.invoke(service, cancelledId, result);
                require(before.equals(store.load()), "cancelled join persisted a late successful profile");
                require(operation.get(service) == null, "cancelled completion retained its owner");
                require(!busy.getBoolean(service), "cancelled completion retained busy state");

                String nextId = PairingOperationId.generate();
                Object next = constructor.newInstance(nextId);
                operation.set(service, next);
                busy.setBoolean(service, true);
                complete.invoke(service, cancelledId, result);
                require(operation.get(service) == next, "stale completion consumed replacement owner");
                require(busy.getBoolean(service), "stale completion released replacement busy state");
                Object nextResult = success.invoke(null, AndroidProfile.fromNative(NativeResponse.objectValue(
                        NativeBridge.nativeCreateProfile("replacement-lifecycle", "test-phone"))));
                complete.invoke(service, nextId, nextResult);
                ProfileCollection saved = ProfileCollection.decode(store.load()).currentCollection();
                require(saved.networks.size() == ProfileCollection.decode(before).currentCollection().networks.size() + 1,
                        "replacement join did not persist exactly one profile");
                require(operation.get(service) == null && !busy.getBoolean(service),
                        "replacement join retained ownership");
                String committed = store.load();
                cancel.invoke(service);
                complete.invoke(service, nextId, nextResult);
                require(committed.equals(store.load()), "post-commit cancellation or duplicate changed profiles");
            } catch (ReflectiveOperationException | P2pVpnException error) {
                throw new AssertionError(error);
            }
        }, 0, TimeUnit.SECONDS).get(30, TimeUnit.SECONDS);
        sendStatus(2, message("cancelled_join", "passed: late success discarded, replacement committed"));
    }

    private void exerciseSupersededStop() throws Exception {
        setVpnConsent(true);
        onMain(() -> require(VpnService.prepare(getTargetContext()) == null,
                "emulator VPN consent is required"));
        Field instance = P2pVpnService.class.getDeclaredField("debugInstance");
        instance.setAccessible(true);
        P2pVpnService service = (P2pVpnService) instance.get(null);
        Method disconnect = P2pVpnService.class.getDeclaredMethod("disconnectRequested", boolean.class);
        disconnect.setAccessible(true);
        String[] stopMethods = {"stopManualService", "finishPairingForegroundService", "stopForMissingLocalNetworkPermission"};
        for (int variant = 0; variant < stopMethods.length * 2; variant++) {
            String name = stopMethods[variant / 2];
            boolean admitBeforePosting = variant % 2 != 0;
            Method stop = P2pVpnService.class.getDeclaredMethod(name);
            stop.setAccessible(true);
            onMain(() -> {
                CountDownLatch entered = new CountDownLatch(1);
                CountDownLatch release = new CountDownLatch(1);
                try {
                    ScheduledFuture<?> pending = currentScope.schedule(() -> {
                        try {
                            entered.countDown();
                            require(release.await(5, TimeUnit.SECONDS), "stop worker was not released");
                            stop.invoke(service);
                        } catch (ReflectiveOperationException | InterruptedException error) {
                            throw new AssertionError(error);
                        }
                    }, 0, TimeUnit.SECONDS);
                    require(entered.await(5, TimeUnit.SECONDS), "stop worker did not become occupied");
                    if (admitBeforePosting) {
                        service.onStartCommand(serviceIntent().setAction(P2pVpnService.ACTION_CONNECT), 0, 101);
                    }
                    release.countDown();
                    pending.get(5, TimeUnit.SECONDS);
                    if (!admitBeforePosting) {
                        service.onStartCommand(serviceIntent().setAction(P2pVpnService.ACTION_CONNECT), 0, 101);
                    }
                } catch (Exception error) {
                    throw new AssertionError(error);
                } finally {
                    release.countDown();
                }
            });
            awaitNativeRunning();
            onMain(() -> {
                require(serviceIsForeground(), "superseded " + name + " removed foreground ownership");
            });
            currentScope.schedule(() -> {
                try {
                    disconnect.invoke(service, true);
                } catch (ReflectiveOperationException error) {
                    throw new AssertionError(error);
                }
            }, 0, TimeUnit.SECONDS).get(10, TimeUnit.SECONDS);
            onMain(() -> require(!serviceIsForeground(), "current stop retained foreground ownership"));
        }
        sendStatus(2, message("superseded_stop", "passed: three stop paths, before and after start admission"));
    }

    private boolean serviceIsForeground() {
        for (ActivityManager.RunningServiceInfo running : getTargetContext()
                .getSystemService(ActivityManager.class).getRunningServices(Integer.MAX_VALUE)) {
            if (running.service.getClassName().equals(P2pVpnService.class.getName())) {
                return running.foreground;
            }
        }
        return false;
    }

    private static Bundle message(String key, String value) {
        Bundle result = new Bundle();
        result.putString(key, value);
        return result;
    }

    private void unbindAndStop() {
        onMain(() -> {
            getTargetContext().stopService(serviceIntent());
            if (connection != null) {
                getTargetContext().unbindService(connection);
                connection = null;
            }
        });
    }

    private void connect() throws IOException {
        setVpnConsent(true);
        onMain(() -> {
            require(VpnService.prepare(getTargetContext()) == null, "emulator VPN consent is required");
            getTargetContext().startForegroundService(serviceIntent().setAction(P2pVpnService.ACTION_CONNECT));
        });
    }

    private void revokeVpn() throws IOException {
        setVpnConsent(false);
        // Re-preparing a no-longer-authorized package releases Android's VPN binding.
        onMain(() -> require(VpnService.prepare(getTargetContext()) != null, "VPN was not revoked"));
    }

    private void setVpnConsent(boolean allowed) throws IOException {
        try (ParcelFileDescriptor.AutoCloseInputStream output = new ParcelFileDescriptor.AutoCloseInputStream(
                getUiAutomation().executeShellCommand("appops set org.hermeticfoundation.p2pvpn.debug ACTIVATE_VPN "
                        + (allowed ? "allow" : "ignore")))) {
            require(output.read() == -1, "unexpected appops output");
        }
    }

    private Intent serviceIntent() { return new Intent(getTargetContext(), P2pVpnService.class); }

    private void onMain(Runnable action) {
        AtomicReference<Throwable> failure = new AtomicReference<>();
        runOnMainSync(() -> {
            try {
                action.run();
            } catch (Throwable error) {
                failure.set(error);
            }
        });
        if (failure.get() != null) {
            throw new AssertionError("main-thread action failed", failure.get());
        }
    }

    private static String nativePhase() {
        try {
            return NativeResponse.objectValue(NativeBridge.nativeStatus()).getString("phase");
        } catch (Exception error) {
            throw new AssertionError("native status failed", error);
        }
    }

    private static void awaitNativeRunning() {
        try {
            await(() -> "running".equals(nativePhase()), 30, "native runtime readiness");
        } catch (AssertionError error) {
            P2pVpnService.Snapshot snapshot = P2pVpnService.debugSnapshot();
            throw new AssertionError("native readiness: "
                    + (snapshot == null ? "service unavailable" : snapshot.connectionDetail), error);
        }
    }

    private static void await(BooleanSupplier condition, int seconds, String description) {
        long deadline = SystemClock.elapsedRealtime() + seconds * 1000L;
        while (!condition.getAsBoolean()) {
            require(SystemClock.elapsedRealtime() < deadline, "timed out: " + description);
            SystemClock.sleep(20);
        }
    }

    private static void require(boolean condition, String message) {
        if (!condition) { throw new AssertionError(message); }
    }
}
