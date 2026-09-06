package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.*;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.Test;

public final class ServiceRuntimeWorkerTest {
    @Test
    public void retirementKeepsCleanupWithoutWaitingForRunningWork() throws Exception {
        ServiceRuntimeWorker.Dispatcher dispatcher = new ServiceRuntimeWorker.Dispatcher();
        CountDownLatch entered = new CountDownLatch(1);
        CountDownLatch release = new CountDownLatch(1);
        AtomicInteger cleanups = new AtomicInteger();
        AtomicInteger unwanted = new AtomicInteger();
        ServiceRuntimeWorker.Scope scope = dispatcher.open(cleanups::incrementAndGet);
        try {
            ScheduledFuture<?> running = scope.schedule(() -> block(entered, release), 0, TimeUnit.SECONDS);
            assertTrue(entered.await(2, TimeUnit.SECONDS));
            ScheduledFuture<?> pending = scope.schedule(unwanted::incrementAndGet, 0, TimeUnit.SECONDS);
            scope.close();
            assertTrue(scope.isClosed());
            assertTrue(pending.isCancelled());
            assertFalse(scope.cleanupCompletion().isDone());
            assertEquals(0, scope.pendingTaskCount());
            scope.execute(unwanted::incrementAndGet);
            assertTrue(scope.schedule(unwanted::incrementAndGet, 1, TimeUnit.DAYS).isCancelled());
            scope.close();
            release.countDown();
            running.get(2, TimeUnit.SECONDS);
            scope.cleanupCompletion().get(2, TimeUnit.SECONDS);
            assertEquals(1, cleanups.get());
            assertEquals(0, unwanted.get());
        } finally {
            release.countDown();
            dispatcher.close();
            assertTrue(dispatcher.awaitTermination(2, TimeUnit.SECONDS));
        }
    }

    @Test
    public void takeoverCleansOldRuntimeBeforeReplacementAndLateCloseIsHarmless() throws Exception {
        ServiceRuntimeWorker.Dispatcher dispatcher = new ServiceRuntimeWorker.Dispatcher();
        CountDownLatch entered = new CountDownLatch(1);
        CountDownLatch release = new CountDownLatch(1);
        List<String> events = new ArrayList<>();
        ServiceRuntimeWorker.Scope old = dispatcher.open(() -> events.add("old cleanup"));
        try {
            old.execute(() -> { block(entered, release); events.add("old returned"); });
            assertTrue(entered.await(2, TimeUnit.SECONDS));
            old.execute(() -> events.add("stale work"));
            ServiceRuntimeWorker.Scope next = dispatcher.open(() -> events.add("new cleanup"));
            assertTrue(old.isClosed());
            ScheduledFuture<?> start = next.schedule(() -> events.add("new start"), 0, TimeUnit.SECONDS);
            release.countDown();
            start.get(2, TimeUnit.SECONDS);
            old.close();
            next.close();
            next.cleanupCompletion().get(2, TimeUnit.SECONDS);
            assertEquals(Arrays.asList("old returned", "old cleanup", "new start", "new cleanup"), events);
        } finally {
            release.countDown();
            dispatcher.close();
            assertTrue(dispatcher.awaitTermination(2, TimeUnit.SECONDS));
        }
    }

    @Test
    public void runningCallbackCannotRearmRetiredScope() throws Exception {
        ServiceRuntimeWorker.Dispatcher dispatcher = new ServiceRuntimeWorker.Dispatcher();
        CountDownLatch entered = new CountDownLatch(1);
        CountDownLatch release = new CountDownLatch(1);
        AtomicReference<ScheduledFuture<?>> rearmed = new AtomicReference<>();
        ServiceRuntimeWorker.Scope scope = dispatcher.open(() -> {});
        try {
            scope.execute(() -> {
                block(entered, release);
                rearmed.set(scope.schedule(() -> fail("retired timer fired"), 0, TimeUnit.SECONDS));
            });
            assertTrue(entered.await(2, TimeUnit.SECONDS));
            dispatcher.close();
            assertFalse(dispatcher.awaitTermination(1, TimeUnit.MILLISECONDS));
            release.countDown();
            assertTrue(dispatcher.awaitTermination(2, TimeUnit.SECONDS));
            scope.cleanupCompletion().get(2, TimeUnit.SECONDS);
            assertTrue(rearmed.get().isCancelled());
        } finally {
            release.countDown();
            dispatcher.close();
            assertTrue(dispatcher.awaitTermination(2, TimeUnit.SECONDS));
        }
    }

    @Test
    public void cancelledTimersAreNotRetained() throws Exception {
        ServiceRuntimeWorker.Dispatcher dispatcher = new ServiceRuntimeWorker.Dispatcher();
        ServiceRuntimeWorker.Scope scope = dispatcher.open(() -> {});
        try {
            for (int i = 0; i < 1000; i++) {
                ScheduledFuture<?> timer = scope.schedule(() -> {}, 1, TimeUnit.DAYS);
                assertEquals(1, scope.pendingTaskCount());
                assertTrue(timer.cancel(false));
                assertEquals(0, scope.pendingTaskCount());
            }
        } finally {
            dispatcher.close();
            assertTrue(dispatcher.awaitTermination(2, TimeUnit.SECONDS));
        }
    }

    @Test
    public void failedWorkDoesNotDiscardCleanup() throws Exception {
        ServiceRuntimeWorker.Dispatcher dispatcher = new ServiceRuntimeWorker.Dispatcher();
        AtomicInteger cleanups = new AtomicInteger();
        ServiceRuntimeWorker.Scope scope = dispatcher.open(cleanups::incrementAndGet);
        try {
            ScheduledFuture<?> task = scope.schedule(() -> { throw new IllegalStateException("failure"); },
                    0, TimeUnit.SECONDS);
            try {
                task.get(2, TimeUnit.SECONDS);
                fail("expected failed task");
            } catch (ExecutionException expected) {
                assertTrue(expected.getCause() instanceof IllegalStateException);
            }
            scope.close();
            scope.cleanupCompletion().get(2, TimeUnit.SECONDS);
            assertEquals(1, cleanups.get());
        } finally {
            dispatcher.close();
            assertTrue(dispatcher.awaitTermination(2, TimeUnit.SECONDS));
        }
    }

    private static void block(CountDownLatch entered, CountDownLatch release) {
        entered.countDown();
        try {
            if (!release.await(5, TimeUnit.SECONDS)) {
                throw new AssertionError("test did not release worker");
            }
        } catch (InterruptedException error) {
            Thread.currentThread().interrupt();
            throw new AssertionError("running work interrupted", error);
        }
    }
}
