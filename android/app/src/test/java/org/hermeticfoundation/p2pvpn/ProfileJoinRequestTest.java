package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

import java.util.ArrayList;
import java.util.List;
import org.junit.Test;

public final class ProfileJoinRequestTest {
    @Test
    public void trimsJoinValuesAndEncodesExistingNetworks() throws Exception {
        ProfileJoinRequest request =
                ProfileJoinRequest.create(
                        "  AB12-CD34  ", "  phone-1  ", List.of("personal", "runners"));

        assertEquals("AB12-CD34", request.pairingCode);
        assertEquals("phone-1", request.hostname);
        assertEquals("[\"personal\",\"runners\"]", request.existingNetworkNamesJson);
    }

    @Test
    public void rejectsMissingOversizedAndControlCharacterValues() {
        assertThrows(
                P2pVpnException.class,
                () -> ProfileJoinRequest.create(null, "phone", List.of()));
        assertThrows(
                P2pVpnException.class,
                () -> ProfileJoinRequest.create("code", " ", List.of()));
        assertThrows(
                P2pVpnException.class,
                () ->
                        ProfileJoinRequest.create(
                                "x".repeat(ProfileJoinRequest.MAX_PAIRING_CODE_LENGTH + 1),
                                "phone",
                                List.of()));
        assertThrows(
                P2pVpnException.class,
                () -> ProfileJoinRequest.create("code\nvalue", "phone", List.of()));
    }

    @Test
    public void rejectsInvalidExistingNetworkSets() {
        ArrayList<String> tooMany = new ArrayList<>();
        for (int index = 0; index <= ProfileCollection.MAX_NETWORKS; index++) {
            tooMany.add("network-" + index);
        }

        assertThrows(
                P2pVpnException.class,
                () -> ProfileJoinRequest.create("code", "phone", tooMany));
        assertThrows(
                P2pVpnException.class,
                () -> ProfileJoinRequest.create("code", "phone", List.of("")));
    }
}
