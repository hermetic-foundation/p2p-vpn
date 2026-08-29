package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public final class ReconnectPolicyTest {
    @Test
    public void firstPhysicalNetworkDoesNotRestartRuntime() {
        ReconnectPolicy policy = new ReconnectPolicy();
        assertEquals(ReconnectPolicy.Change.INITIAL, policy.observe("network-1", 100));
        assertEquals(ReconnectPolicy.Change.UNCHANGED, policy.observe("network-1", 100));
    }

    @Test
    public void betterPhysicalNetworkRequestsReconnect() {
        ReconnectPolicy policy = new ReconnectPolicy();
        policy.observe("cellular", 100);
        assertEquals(ReconnectPolicy.Change.RECONNECT, policy.observe("wifi", 200));
    }

    @Test
    public void lowerPriorityNetworkDoesNotDisplaceCurrentNetwork() {
        ReconnectPolicy policy = new ReconnectPolicy();
        policy.observe("wifi", 200);
        assertEquals(ReconnectPolicy.Change.UNCHANGED, policy.observe("cellular", 100));
    }

    @Test
    public void onlyCurrentNetworkLossRequestsReconnect() {
        ReconnectPolicy policy = new ReconnectPolicy();
        policy.observe("network-1", 100);
        assertFalse(policy.lost("network-2"));
        assertTrue(policy.lost("network-1"));
        assertFalse(policy.lost("network-1"));
    }

    @Test
    public void replacementDoesNotReactAgainToOldNetworkLoss() {
        ReconnectPolicy policy = new ReconnectPolicy();
        policy.observe("cellular", 100);
        assertEquals(ReconnectPolicy.Change.RECONNECT, policy.observe("wifi", 200));
        assertFalse(policy.lost("cellular"));
        assertTrue(policy.lost("wifi"));
    }

    @Test
    public void networkAfterTotalLossRequestsRecovery() {
        ReconnectPolicy policy = new ReconnectPolicy();
        policy.observe("wifi", 200);
        assertTrue(policy.lost("wifi"));
        assertEquals(ReconnectPolicy.Change.RECONNECT, policy.observe("cellular", 100));
    }

    @Test
    public void currentLossPromotesAvailableFallback() {
        ReconnectPolicy policy = new ReconnectPolicy();
        policy.observe("wifi", 200);
        policy.observe("cellular", 100);
        assertTrue(policy.lost("wifi"));
        assertEquals(ReconnectPolicy.Change.UNCHANGED, policy.observe("cellular", 100));
    }

    @Test
    public void equalPriorityCallbacksDoNotCauseChurn() {
        ReconnectPolicy policy = new ReconnectPolicy();
        policy.observe("wifi-2", 200);
        assertEquals(ReconnectPolicy.Change.UNCHANGED, policy.observe("wifi-1", 200));
    }

    @Test
    public void missingNetworkCallbacksCannotTriggerRecovery() {
        ReconnectPolicy policy = new ReconnectPolicy();
        assertEquals(ReconnectPolicy.Change.UNCHANGED, policy.observe(null, 100));
        assertEquals(ReconnectPolicy.Change.UNCHANGED, policy.observe("", 100));
        assertEquals(ReconnectPolicy.Change.UNCHANGED, policy.observe("invalid", -1));
        assertFalse(policy.lost("vpn"));
    }
}
