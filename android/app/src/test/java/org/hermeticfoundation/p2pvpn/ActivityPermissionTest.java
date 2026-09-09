package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.*;

import android.app.Activity;
import java.lang.reflect.Field;
import org.junit.Test;

public final class ActivityPermissionTest {
    @Test
    public void denialDiscardsPendingEnableAndLaterSuccessCannotReviveIt() throws Exception {
        MainActivity activity = new MainActivity();
        set(activity, "pendingEnableNetworkId", "cancelled-network");
        activity.onActivityResult(requestCode(), Activity.RESULT_CANCELED, null);
        assertNull(get(activity, "pendingEnableNetworkId"));
        assertNull(get(activity, "pendingMutationNetworkId"));
        activity.onActivityResult(requestCode(), Activity.RESULT_OK, null);
        assertNull("late permission result dispatched an activation", get(activity, "pendingMutationNetworkId"));
    }

    @Test
    public void successConsumesExactlyTheRequestedNetworkAndDuplicatesAreInert() throws Exception {
        MainActivity activity = new MainActivity();
        set(activity, "pendingEnableNetworkId", "requested-network");
        activity.onActivityResult(requestCode(), Activity.RESULT_OK, null);
        assertNull(get(activity, "pendingEnableNetworkId"));
        assertEquals("requested-network", get(activity, "pendingMutationNetworkId"));
        assertEquals(Boolean.TRUE, get(activity, "pendingMutationEnabled"));
        set(activity, "pendingMutationNetworkId", null);
        set(activity, "pendingMutationEnabled", null);
        activity.onActivityResult(requestCode(), Activity.RESULT_OK, null);
        assertNull(get(activity, "pendingMutationNetworkId"));
        assertNull(get(activity, "pendingMutationEnabled"));
    }

    @Test
    public void unrelatedResultDoesNotConsumePermissionOwner() throws Exception {
        MainActivity activity = new MainActivity();
        set(activity, "pendingEnableNetworkId", "waiting-network");
        activity.onActivityResult(-1, Activity.RESULT_OK, null);
        assertEquals("waiting-network", get(activity, "pendingEnableNetworkId"));
        assertNull(get(activity, "pendingMutationNetworkId"));
    }

    private static int requestCode() throws Exception {
        Field field = MainActivity.class.getDeclaredField("VPN_PERMISSION_REQUEST");
        field.setAccessible(true);
        return field.getInt(null);
    }

    private static Object get(MainActivity activity, String name) throws Exception {
        Field field = MainActivity.class.getDeclaredField(name);
        field.setAccessible(true);
        return field.get(activity);
    }

    private static void set(MainActivity activity, String name, Object value) throws Exception {
        Field field = MainActivity.class.getDeclaredField(name);
        field.setAccessible(true);
        field.set(activity, value);
    }
}
