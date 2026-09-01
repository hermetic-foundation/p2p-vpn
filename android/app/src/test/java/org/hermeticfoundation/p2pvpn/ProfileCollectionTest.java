package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotEquals;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public final class ProfileCollectionTest {
    private static final String FIRST_ID = "11111111-1111-4111-8111-111111111111";
    private static final String SECOND_ID = "22222222-2222-4222-8222-222222222222";

    @Test
    public void legacyConfigIsPreservedWithoutReencoding() throws Exception {
        String legacy = " {\n  \"network\": {\"name\": \"legacy\"}\n} ";

        ProfileCollection.Decoded decoded = ProfileCollection.decode(legacy);

        assertTrue(decoded.isLegacy());
        assertEquals(legacy, decoded.legacyConfigJson);
        assertNull(decoded.collection);
    }

    @Test
    public void collectionRoundTripsSelectionOrderAndEnabledState() throws Exception {
        ProfileCollection collection =
                ProfileCollection.single(entry(FIRST_ID, true, "alpha"))
                        .add(entry(SECOND_ID, false, "beta"), true);

        ProfileCollection.Decoded decoded = ProfileCollection.decode(collection.toJson());

        assertFalse(decoded.isLegacy());
        assertEquals(SECOND_ID, decoded.collection.selectedNetworkId);
        assertEquals(2, decoded.collection.networks.size());
        assertEquals(FIRST_ID, decoded.collection.networks.get(0).id);
        assertTrue(decoded.collection.networks.get(0).enabled);
        assertEquals(SECOND_ID, decoded.collection.networks.get(1).id);
        assertFalse(decoded.collection.networks.get(1).enabled);
        assertEquals(config("beta"), decoded.collection.networks.get(1).configJson);
    }

    @Test
    public void migratedIdIsStableAndNetworkScoped() throws Exception {
        String first = ProfileCollection.migratedNetworkId("personal", "12D3KooWPeer");
        String repeated = ProfileCollection.migratedNetworkId("personal", "12D3KooWPeer");
        String otherNetwork = ProfileCollection.migratedNetworkId("runners", "12D3KooWPeer");

        assertEquals(first, repeated);
        assertNotEquals(first, otherNetwork);
    }

    @Test
    public void migrationKeepsConfigAndEnablesTheExistingNetwork() throws Exception {
        String config = config("legacy");

        ProfileCollection migrated =
                ProfileCollection.migrated(config, "legacy", "12D3KooWLegacyPeer");

        assertEquals(1, migrated.networks.size());
        assertEquals(migrated.selectedNetworkId, migrated.selected().id);
        assertTrue(migrated.selected().enabled);
        assertEquals(config, migrated.selected().configJson);
    }

    @Test
    public void mutationsRemainImmutableAndRepairSelectionAfterRemoval() throws Exception {
        ProfileCollection original =
                ProfileCollection.single(entry(FIRST_ID, true, "alpha"))
                        .add(entry(SECOND_ID, true, "beta"), true);

        ProfileCollection disabled =
                original.replace(original.selected().withEnabled(false));
        ProfileCollection removed = disabled.remove(SECOND_ID);

        assertTrue(original.selected().enabled);
        assertFalse(disabled.selected().enabled);
        assertEquals(FIRST_ID, removed.selectedNetworkId);
        assertEquals(1, removed.networks.size());
    }

    @Test
    public void duplicateAndUnknownIdentifiersAreRejected() throws Exception {
        ProfileCollection collection = ProfileCollection.single(entry(FIRST_ID, true, "alpha"));

        assertThrows(
                P2pVpnException.class,
                () -> collection.add(entry(FIRST_ID, false, "duplicate"), false));
        assertThrows(P2pVpnException.class, () -> collection.select(SECOND_ID));
        assertThrows(P2pVpnException.class, () -> collection.replace(entry(SECOND_ID, true, "x")));
    }

    @Test
    public void collectionCountAndConfigSizeAreBounded() throws Exception {
        ProfileCollection collection = ProfileCollection.single(entry(FIRST_ID, true, "alpha"));
        for (int index = 1; index < ProfileCollection.MAX_NETWORKS; index++) {
            String id = String.format("%08x-0000-4000-8000-%012x", index, index);
            collection = collection.add(entry(id, true, "network-" + index), false);
        }
        ProfileCollection full = collection;

        assertThrows(
                P2pVpnException.class,
                () -> full.add(entry("ffffffff-ffff-4fff-8fff-ffffffffffff", true, "extra"), false));
        assertThrows(
                P2pVpnException.class,
                () ->
                        new ProfileCollection.Entry(
                                FIRST_ID,
                                true,
                                "x".repeat(ProfileCollection.MAX_CONFIG_BYTES + 1)));
    }

    @Test
    public void malformedCollectionDoesNotFallBackToLegacy() {
        String malformed =
                "{\"kind\":\""
                        + ProfileCollection.KIND
                        + "\",\"schema_version\":99,\"networks\":[]}";

        assertThrows(P2pVpnException.class, () -> ProfileCollection.decode(malformed));
    }

    private static ProfileCollection.Entry entry(
            String id, boolean enabled, String networkName) throws Exception {
        return new ProfileCollection.Entry(id, enabled, config(networkName));
    }

    private static String config(String networkName) {
        return "{\"network\":{\"name\":\"" + networkName + "\"}}";
    }
}
