package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public final class VpnModeTest {
    @Test
    public void manualModePermitsConnectAndDisconnect() {
        VpnMode mode = VpnMode.resolve(37, false, false, false);

        assertFalse(mode.alwaysOn);
        assertFalse(mode.lockdown);
        assertTrue(mode.permitsDisconnect());
        assertTrue(mode.permitsOverlayConnection());
    }

    @Test
    public void platformAlwaysOnPreventsAppDisconnect() {
        VpnMode mode = VpnMode.resolve(37, false, true, false);

        assertTrue(mode.alwaysOn);
        assertFalse(mode.lockdown);
        assertFalse(mode.permitsDisconnect());
        assertTrue(mode.permitsOverlayConnection());
    }

    @Test
    public void lockdownImpliesAlwaysOnAndRejectsSplitTunnel() {
        VpnMode mode = VpnMode.resolve(37, false, false, true);

        assertTrue(mode.alwaysOn);
        assertTrue(mode.lockdown);
        assertFalse(mode.permitsDisconnect());
        assertFalse(mode.permitsOverlayConnection());
    }

    @Test
    public void legacySystemStartIdentifiesAlwaysOnWithoutInspectionApi() {
        VpnMode mode = VpnMode.resolve(28, true, false, false);

        assertTrue(mode.alwaysOn);
        assertFalse(mode.lockdown);
        assertFalse(mode.permitsDisconnect());
    }

    @Test
    public void modernPlatformStateOverridesStartOrigin() {
        VpnMode mode = VpnMode.resolve(37, true, false, false);

        assertFalse(mode.alwaysOn);
        assertTrue(mode.permitsDisconnect());
    }
}
