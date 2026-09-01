package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public final class ServiceRestartPolicyTest {
    @Test
    public void connectStartRequestsProcessDeathRecovery() {
        assertTrue(
                P2pVpnService.shouldRestartAfterProcessDeath(
                        P2pVpnService.ACTION_CONNECT, false, false, false));
    }

    @Test
    public void systemVpnStartRequestsProcessDeathRecovery() {
        assertTrue(P2pVpnService.shouldRestartAfterProcessDeath(null, true, false, false));
    }

    @Test
    public void activeConnectionKeepsDebugStartsSticky() {
        assertTrue(
                P2pVpnService.shouldRestartAfterProcessDeath(
                        P2pVpnService.ACTION_DEBUG_COMMAND, false, true, false));
        assertTrue(
                P2pVpnService.shouldRestartAfterProcessDeath(
                        P2pVpnService.ACTION_DEBUG_COMMAND, false, false, true));
    }

    @Test
    public void networkActivationKeepsTheRequestedVpnSticky() {
        assertTrue(
                P2pVpnService.shouldRestartAfterProcessDeath(
                        P2pVpnService.ACTION_SET_NETWORK_ENABLED, false, false, false));
    }

    @Test
    public void idleDebugStartDoesNotKeepServiceAlive() {
        assertFalse(
                P2pVpnService.shouldRestartAfterProcessDeath(
                        P2pVpnService.ACTION_DEBUG_COMMAND, false, false, false));
    }

    @Test
    public void explicitDisconnectCancelsProcessDeathRecovery() {
        assertFalse(
                P2pVpnService.shouldRestartAfterProcessDeath(
                        P2pVpnService.ACTION_DISCONNECT, true, true, true));
    }
}
