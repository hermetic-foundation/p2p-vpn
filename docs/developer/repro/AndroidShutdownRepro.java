import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Future;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.ScheduledThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;

/** Characterizes the reviewed executor pattern, not an Android device or JNI runtime. */
public final class AndroidShutdownRepro {
    public static void main(String[] args) throws Exception {
        discardedCleanup();
        staleUnscopedCleanup();
    }

    private static void discardedCleanup() throws Exception {
        ScheduledThreadPoolExecutor worker = new ScheduledThreadPoolExecutor(1);
        worker.setRemoveOnCancelPolicy(true);
        CountDownLatch entered = new CountDownLatch(1);
        CountDownLatch release = new CountDownLatch(1);
        AtomicBoolean cleaned = new AtomicBoolean();
        try {
            worker.execute(() -> blockingCall(entered, release));
            require(entered.await(2, TimeUnit.SECONDS), "worker did not start");
            Future<?> cleanup = worker.submit(() -> cleaned.set(true));
            try {
                cleanup.get(6, TimeUnit.SECONDS);
                throw new AssertionError("cleanup unexpectedly ran behind blocked work");
            } catch (TimeoutException expected) {
                // Same bounded wait followed by shutdownNow as the reviewed service.
            }
            List<Runnable> discarded = worker.shutdownNow();
            require(discarded.contains(cleanup), "cleanup was not in the discarded queue");
            release.countDown();
            require(worker.awaitTermination(2, TimeUnit.SECONDS), "worker did not terminate");
            require(!cleaned.get(), "cleanup unexpectedly ran");
            require(!cleanup.isDone(), "discarded cleanup unexpectedly completed");
            boolean rejected = false;
            try {
                worker.execute(() -> {});
            } catch (RejectedExecutionException expected) {
                rejected = true;
            }
            require(rejected, "late work was not rejected");
            System.out.println("discarded_cleanup=1 cleanup_ran=false late_work_rejected=true");
        } finally {
            release.countDown();
            worker.shutdownNow();
            require(worker.awaitTermination(2, TimeUnit.SECONDS), "reproducer worker leaked");
        }
    }

    private static void staleUnscopedCleanup() throws Exception {
        ScheduledThreadPoolExecutor oldWorker = new ScheduledThreadPoolExecutor(1);
        CountDownLatch entered = new CountDownLatch(1);
        CountDownLatch release = new CountDownLatch(1);
        AtomicInteger processRuntime = new AtomicInteger(1);
        try {
            oldWorker.execute(() -> blockingCall(entered, release));
            require(entered.await(2, TimeUnit.SECONDS), "old worker did not start");
            Future<Integer> cleanup = oldWorker.submit(() -> processRuntime.getAndSet(0));
            oldWorker.shutdown();
            processRuntime.set(2);
            release.countDown();
            require(cleanup.get(2, TimeUnit.SECONDS) == 2, "unexpected removed generation");
            require(processRuntime.get() == 0, "replacement runtime unexpectedly survived");
            System.out.println("graceful_old_cleanup_removed_generation=2 replacement_running=false");
        } finally {
            release.countDown();
            oldWorker.shutdownNow();
            require(oldWorker.awaitTermination(2, TimeUnit.SECONDS), "old worker leaked");
        }
    }

    private static void blockingCall(CountDownLatch entered, CountDownLatch release) {
        entered.countDown();
        boolean interrupted = false;
        for (;;) {
            try {
                release.await();
                break;
            } catch (InterruptedException ignored) {
                // Model work that does not finish merely because Java requests interruption.
                interrupted = true;
            }
        }
        if (interrupted) {
            Thread.currentThread().interrupt();
        }
    }

    private static void require(boolean condition, String message) {
        if (!condition) {
            throw new AssertionError(message);
        }
    }
}
