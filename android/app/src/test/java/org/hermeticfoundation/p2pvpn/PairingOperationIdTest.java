package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotEquals;
import static org.junit.Assert.assertTrue;

import java.util.Base64;
import org.junit.Test;

public final class PairingOperationIdTest {
    @Test
    public void generatesCanonicalProtocolOperationIds() {
        String first = PairingOperationId.generate();
        String second = PairingOperationId.generate();

        assertEquals(22, first.length());
        assertTrue(first.matches("[A-Za-z0-9_-]+"));
        assertFalse(first.contains("="));
        assertEquals(16, Base64.getUrlDecoder().decode(first).length);
        assertNotEquals(first, second);
    }
}
