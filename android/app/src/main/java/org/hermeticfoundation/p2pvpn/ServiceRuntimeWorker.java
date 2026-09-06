package org.hermeticfoundation.p2pvpn;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.Objects;
import java.util.Set;
import java.util.concurrent.Delayed;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.Future;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.ScheduledThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;

/** Serializes process-global native operations while retiring service-local work. */
final class ServiceRuntimeWorker {
    private static final Dispatcher PROCESS = new Dispatcher();

    static Scope open(Runnable cleanup) {
        return PROCESS.open(cleanup);
    }

    static final class Dispatcher implements AutoCloseable {
        private final ScheduledThreadPoolExecutor executor;
        private Scope current;
        private boolean closed;

        Dispatcher() {
            executor = new ScheduledThreadPoolExecutor(1, runnable -> {
                Thread thread = new Thread(runnable, "p2p-vpn-runtime-worker");
                thread.setDaemon(true);
                return thread;
            });
            executor.setRemoveOnCancelPolicy(true);
            executor.setExecuteExistingDelayedTasksAfterShutdownPolicy(false);
            executor.setKeepAliveTime(30, TimeUnit.SECONDS);
            executor.allowCoreThreadTimeOut(true);
        }

        synchronized Scope open(Runnable cleanup) {
            Objects.requireNonNull(cleanup);
            if (closed) {
                throw new IllegalStateException("runtime dispatcher is closed");
            }
            if (current != null) {
                current.closeLocked();
            }
            current = new Scope(this, cleanup);
            return current;
        }

        @Override
        public synchronized void close() {
            if (!closed) {
                if (current != null) {
                    current.closeLocked();
                }
                closed = true;
                executor.shutdown();
            }
        }

        boolean awaitTermination(long timeout, TimeUnit unit) throws InterruptedException {
            return executor.awaitTermination(timeout, unit);
        }
    }

    static final class Scope implements AutoCloseable {
        private final Dispatcher dispatcher;
        private final Runnable cleanup;
        private final Set<Task> pending = new HashSet<>();
        private volatile boolean closed;
        private Future<?> cleanupFuture;

        private Scope(Dispatcher dispatcher, Runnable cleanup) {
            this.dispatcher = dispatcher;
            this.cleanup = cleanup;
        }

        boolean isClosed() {
            return closed;
        }

        void execute(Runnable action) {
            schedule(action, 0, TimeUnit.MILLISECONDS);
        }

        ScheduledFuture<?> schedule(Runnable action, long delay, TimeUnit unit) {
            Objects.requireNonNull(action);
            Objects.requireNonNull(unit);
            synchronized (dispatcher) {
                Task task = new Task(this, action);
                if (closed) {
                    task.future = new java.util.concurrent.FutureTask<Void>(() -> null);
                    task.future.cancel(false);
                    return task;
                }
                pending.add(task);
                task.future = dispatcher.executor.schedule(task, delay, unit);
                return task;
            }
        }

        @Override
        public void close() {
            synchronized (dispatcher) {
                closeLocked();
            }
        }

        Future<?> cleanupCompletion() {
            synchronized (dispatcher) {
                return cleanupFuture;
            }
        }

        int pendingTaskCount() {
            synchronized (dispatcher) {
                return pending.size();
            }
        }

        private void closeLocked() {
            if (closed) {
                return;
            }
            closed = true;
            if (dispatcher.current == this) {
                dispatcher.current = null;
            }
            for (Task task : new ArrayList<>(pending)) {
                task.cancel(false);
            }
            // Never interrupt running native work or discard its following cleanup.
            cleanupFuture = dispatcher.executor.submit(cleanup);
        }
    }

    private static final class Task implements Runnable, ScheduledFuture<Void> {
        private final Scope scope;
        private final Runnable action;
        private Future<?> future;

        Task(Scope scope, Runnable action) {
            this.scope = scope;
            this.action = action;
        }

        @Override
        public void run() {
            synchronized (scope.dispatcher) {
                scope.pending.remove(this);
                if (scope.closed) {
                    return;
                }
            }
            action.run();
        }

        @Override
        public boolean cancel(boolean interrupt) {
            synchronized (scope.dispatcher) {
                scope.pending.remove(this);
                return future.cancel(interrupt);
            }
        }

        @Override
        public boolean isCancelled() {
            return future.isCancelled();
        }

        @Override
        public boolean isDone() {
            return future.isDone();
        }

        @Override
        public Void get() throws InterruptedException, ExecutionException {
            future.get();
            return null;
        }

        @Override
        public Void get(long timeout, TimeUnit unit)
                throws InterruptedException, ExecutionException, TimeoutException {
            future.get(timeout, unit);
            return null;
        }

        @Override
        public long getDelay(TimeUnit unit) {
            return future instanceof ScheduledFuture<?>
                    ? ((ScheduledFuture<?>) future).getDelay(unit) : 0;
        }

        @Override
        public int compareTo(Delayed other) {
            return Long.compare(getDelay(TimeUnit.NANOSECONDS), other.getDelay(TimeUnit.NANOSECONDS));
        }
    }
}
