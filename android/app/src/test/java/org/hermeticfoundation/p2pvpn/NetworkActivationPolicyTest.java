package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

import org.junit.Test;

public final class NetworkActivationPolicyTest {
    @Test
    public void anyEnabledNetworkRequestsTheSharedVpn() {
        assertEquals(
                NetworkActivationPolicy.Outcome.CONNECT,
                NetworkActivationPolicy.afterMutation(1, false));
        assertEquals(
                NetworkActivationPolicy.Outcome.CONNECT,
                NetworkActivationPolicy.afterMutation(16, true));
    }

    @Test
    public void emptyEnabledSetStopsOnlyManualVpn() {
        assertEquals(
                NetworkActivationPolicy.Outcome.STOP,
                NetworkActivationPolicy.afterMutation(0, false));
        assertEquals(
                NetworkActivationPolicy.Outcome.IDLE_ALWAYS_ON,
                NetworkActivationPolicy.afterMutation(0, true));
    }

    @Test
    public void invalidCountsFailClosed() {
        assertThrows(
                IllegalArgumentException.class,
                () -> NetworkActivationPolicy.afterMutation(-1, false));
        assertThrows(
                IllegalArgumentException.class,
                () ->
                        NetworkActivationPolicy.afterMutation(
                                ProfileCollection.MAX_NETWORKS + 1, false));
    }
}
