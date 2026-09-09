package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.*;

import android.content.ServiceConnection;
import java.lang.reflect.Field;
import java.util.Set;
import org.junit.Test;

public final class ActivityBindingTest {
    @Test
    public void unsuccessfulBindingStillOwnsCleanupUntilStop() throws Exception {
        MainActivity activity = new MainActivity();
        // The Android JVM stub returns false from bindService.
        activity.onStart();
        try {
            assertTrue("failed binding lost cleanup ownership", (boolean) get(activity, "bindingRegistered"));
            assertFalse((boolean) get(activity, "bound"));
            assertNotNull(get(activity, "serviceConnection"));
        } finally {
            activity.onStop();
        }
        assertFalse((boolean) get(activity, "bindingRegistered"));
        assertNull(get(activity, "serviceConnection"));
    }

    @Test
    public void stoppedActivityRejectsLateBindingCallback() throws Exception {
        MainActivity activity = new MainActivity();
        activity.onStart();
        ServiceConnection connection = (ServiceConnection) get(activity, "serviceConnection");
        activity.onStop();
        P2pVpnService service = new P2pVpnService();
        try {
            connection.onServiceConnected(null, service.new LocalBinder());
            assertFalse("late callback attached a stopped activity", (boolean) get(activity, "bound"));
            assertNull(get(activity, "binder"));
            assertTrue("stopped activity leaked a listener", ((Set<?>) get(service, "listeners")).isEmpty());
        } finally {
            activity.onStop();
        }
    }

    @Test
    public void replacementBindingSurvivesOldCallbacksAndSnapshots() throws Exception {
        MainActivity activity = new MainActivity();
        P2pVpnService service = new P2pVpnService();
        activity.onStart();
        ServiceConnection old = (ServiceConnection) get(activity, "serviceConnection");
        // JVM Android stubs do not register bindings; admit the controlled callback explicitly.
        set(activity, "bindingRegistered", true);
        old.onServiceConnected(null, service.new LocalBinder());
        P2pVpnService.Listener oldListener =
                (P2pVpnService.Listener) ((Set<?>) get(service, "listeners")).iterator().next();
        activity.onStop();
        assertTrue(((Set<?>) get(service, "listeners")).isEmpty());
        activity.onStart();
        ServiceConnection next = (ServiceConnection) get(activity, "serviceConnection");
        assertNotSame(old, next);
        set(activity, "bindingRegistered", true);
        P2pVpnService.LocalBinder nextBinder = service.new LocalBinder();
        try {
            next.onServiceConnected(null, nextBinder);
            old.onServiceConnected(null, service.new LocalBinder());
            old.onServiceDisconnected(null);
            oldListener.onSnapshot((P2pVpnService.Snapshot) get(service, "snapshot"));
            assertSame(nextBinder, get(activity, "binder"));
            assertTrue((boolean) get(activity, "bound"));
            assertNull("retired listener published a snapshot", get(activity, "latestSnapshot"));
            assertEquals(1, ((Set<?>) get(service, "listeners")).size());
            next.onServiceDisconnected(null);
            assertFalse((boolean) get(activity, "bound"));
            assertTrue("disconnect lost registration ownership", (boolean) get(activity, "bindingRegistered"));
            next.onServiceConnected(null, nextBinder);
            assertTrue("current binding failed to reconnect", (boolean) get(activity, "bound"));
        } finally {
            activity.onStop();
        }
        assertFalse((boolean) get(activity, "bindingRegistered"));
        assertTrue(((Set<?>) get(service, "listeners")).isEmpty());
    }

    private static void set(Object object, String name, Object value) throws Exception {
        Field field = object.getClass().getDeclaredField(name);
        field.setAccessible(true);
        field.set(object, value);
    }

    private static Object get(Object object, String name) throws Exception {
        Field field = object.getClass().getDeclaredField(name);
        field.setAccessible(true);
        return field.get(object);
    }
}
