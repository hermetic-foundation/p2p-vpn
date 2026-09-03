package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertThrows;

import org.json.JSONObject;
import org.junit.Test;

public final class MembershipMutationTest {
    @Test
    public void parsesCompleteMutationResult() throws Exception {
        MembershipMutation.Result result =
                MembershipMutation.Result.from(
                        new JSONObject()
                                .put("member_peer", "memberPeer")
                                .put("issuer_peer", "issuerPeer")
                                .put("membership_epoch", 2)
                                .put("sequence", 9)
                                .put("resigned", false));

        assertEquals("memberPeer", result.memberPeer);
        assertEquals("issuerPeer", result.issuerPeer);
        assertEquals(2, result.membershipEpoch);
        assertEquals(9, result.sequence);
        assertFalse(result.resigned);
    }

    @Test
    public void rejectsMalformedMutationResults() throws Exception {
        assertMalformed(new JSONObject());
        assertMalformed(validResult().put("member_peer", "invalid peer"));
        assertMalformed(validResult().put("issuer_peer", ""));
        assertMalformed(validResult().put("membership_epoch", -1));
        assertMalformed(validResult().put("membership_epoch", "2"));
        assertMalformed(validResult().put("sequence", -1));
        assertMalformed(validResult().put("sequence", 1.5));
        assertMalformed(validResult().put("resigned", "false"));
    }

    private static JSONObject validResult() throws Exception {
        return new JSONObject()
                .put("member_peer", "memberPeer")
                .put("issuer_peer", "issuerPeer")
                .put("membership_epoch", 2)
                .put("sequence", 9)
                .put("resigned", false);
    }

    private static void assertMalformed(JSONObject value) {
        assertThrows(P2pVpnException.class, () -> MembershipMutation.Result.from(value));
    }
}
