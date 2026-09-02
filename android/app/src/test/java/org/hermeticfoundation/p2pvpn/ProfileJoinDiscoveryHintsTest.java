package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

import org.junit.After;
import org.junit.Test;

public final class ProfileJoinDiscoveryHintsTest {
    @After
    public void clearHint() {
        ProfileJoinDiscoveryHints.consumeNextForDebug();
    }

    @Test
    public void debugHintIsBoundedAndConsumedOnce() {
        ProfileJoinDiscoveryHints.setNextForDebug(
                "peer-id", "/ip4/10.0.2.2/tcp/42300/p2p/peer-id");

        assertEquals(
                "[{\"id\":\"peer-id\",\"address\":\"/ip4/10.0.2.2/tcp/42300/p2p/peer-id\"}]",
                ProfileJoinDiscoveryHints.consumeNextForDebug());
        assertEquals("[]", ProfileJoinDiscoveryHints.consumeNextForDebug());
    }

    @Test
    public void debugHintRejectsControlCharacters() {
        assertThrows(
                IllegalArgumentException.class,
                () -> ProfileJoinDiscoveryHints.setNextForDebug("peer-id", "/ip4/127.0.0.1\n"));
    }
}
