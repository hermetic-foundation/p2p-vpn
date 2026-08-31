package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public final class LocalNetworkPermissionTest {
    @Test
    public void requiresPermissionOnlyWhenDeviceAndTargetEnforceIt() {
        assertFalse(LocalNetworkPermission.isRequired(36, 37));
        assertFalse(LocalNetworkPermission.isRequired(37, 36));
        assertTrue(LocalNetworkPermission.isRequired(37, 37));
        assertTrue(LocalNetworkPermission.isRequired(38, 37));
    }
}
