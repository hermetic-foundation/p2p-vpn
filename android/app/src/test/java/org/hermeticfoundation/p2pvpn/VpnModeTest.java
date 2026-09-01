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

    @Test
    public void activeAlwaysOnOwnershipSurvivesANotRunningObservation() {
        VpnMode previous = VpnMode.resolve(37, false, true, true);
        VpnMode observed = VpnMode.resolve(37, false, false, false);

        VpnMode stabilized = VpnMode.stabilize(37, previous, observed, true);

        assertTrue(stabilized.alwaysOn);
        assertTrue(stabilized.lockdown);
    }

    @Test
    public void positivePlatformObservationReleasesLockdown() {
        VpnMode previous = VpnMode.resolve(37, false, true, true);
        VpnMode observed = VpnMode.resolve(37, false, true, false);

        VpnMode stabilized = VpnMode.stabilize(37, previous, observed, true);

        assertTrue(stabilized.alwaysOn);
        assertFalse(stabilized.lockdown);
    }

    @Test
    public void inactiveConnectionDoesNotRetainPlatformOwnership() {
        VpnMode previous = VpnMode.resolve(37, false, true, true);
        VpnMode observed = VpnMode.resolve(37, false, false, false);

        VpnMode stabilized = VpnMode.stabilize(37, previous, observed, false);

        assertFalse(stabilized.alwaysOn);
        assertFalse(stabilized.lockdown);
    }

    @Test
    public void preEventAndroidUsesPollingForLockdownRelease() {
        VpnMode previous = VpnMode.resolve(32, false, true, true);
        VpnMode observed = VpnMode.resolve(32, false, false, false);

        VpnMode stabilized = VpnMode.stabilize(32, previous, observed, true);

        assertFalse(stabilized.alwaysOn);
        assertFalse(stabilized.lockdown);
    }
}
