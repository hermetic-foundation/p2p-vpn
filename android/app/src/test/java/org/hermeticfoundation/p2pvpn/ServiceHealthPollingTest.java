package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.*;

import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;
import org.junit.Test;

public final class ServiceHealthPollingTest {
    @Test
    public void connectedModeEventRetainsHealthPolling() throws Exception {
        ServiceRuntimeWorker.Dispatcher dispatcher = new ServiceRuntimeWorker.Dispatcher();
        ServiceRuntimeWorker.Scope scope = dispatcher.open(() -> {});
        try {
            P2pVpnService service = new P2pVpnService();
            set(service, "worker", scope);
            set(service, "connected", true);
            set(service, "desiredConnected", true);
            ScheduledFuture<?> previous = scope.schedule(() -> {}, 1, TimeUnit.DAYS);
            set(service, "statusFuture", previous);
            Method changed = P2pVpnService.class.getDeclaredMethod("vpnManagerModeChanged", VpnMode.class);
            changed.setAccessible(true);
            scope.schedule(() -> {
                try {
                    ScheduledFuture<?> before = previous;
                    for (int event = 0; event < 3; event++) {
                        changed.invoke(service, VpnMode.manual());
                        ScheduledFuture<?> next = (ScheduledFuture<?>) get(service, "statusFuture");
                        assertNotNull("connected mode event must retain a health poll", next);
                        assertFalse(next.isCancelled());
                        assertFalse(next.isDone());
                        assertTrue(before == next || before.isCancelled());
                        assertTrue("mode events must not accumulate timers", scope.pendingTaskCount() <= 2);
                        before = next;
                    }
                } catch (ReflectiveOperationException error) {
                    throw new AssertionError(error);
                }
            }, 0, TimeUnit.SECONDS).get(2, TimeUnit.SECONDS);
        } finally {
            dispatcher.close();
            assertTrue(dispatcher.awaitTermination(2, TimeUnit.SECONDS));
        }
    }

    private static void set(P2pVpnService service, String name, Object value) throws Exception {
        Field field = P2pVpnService.class.getDeclaredField(name);
        field.setAccessible(true);
        field.set(service, value);
    }

    private static Object get(P2pVpnService service, String name) throws ReflectiveOperationException {
        Field field = P2pVpnService.class.getDeclaredField(name);
        field.setAccessible(true);
        return field.get(service);
    }
}
