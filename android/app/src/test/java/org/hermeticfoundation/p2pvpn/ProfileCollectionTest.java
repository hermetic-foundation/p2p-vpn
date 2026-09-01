package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotEquals;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

public final class ProfileCollectionTest {
    private static final String FIRST_ID = "11111111-1111-4111-8111-111111111111";
    private static final String SECOND_ID = "22222222-2222-4222-8222-222222222222";
    private static final String IPV4 = "10.42.0.9";
    private static final String IPV6 = "fd42::9";

    @Test
    public void rawLegacyMigrationUsesExplicitStateAndPreservesInput() throws Exception {
        String legacy = " {\n  \"network\": {\"name\": \"legacy\"}\n} ";
        AndroidProfile selected = profile(legacy, "legacy", IPV4, "fd42:0:0:0:0:0:0:9");

        ProfileCollection.Decoded decoded = ProfileCollection.decode(legacy);

        assertEquals(ProfileCollection.Decoded.State.LEGACY_PROFILE, decoded.state);
        assertTrue(decoded.needsMigration());
        assertEquals(legacy, decoded.legacyConfigJson());
        assertThrows(IllegalStateException.class, decoded::currentCollection);

        ProfileCollection migrated =
                ProfileCollection.migrated(
                        decoded.legacyConfigJson(),
                        selected.networkName,
                        selected.peerId,
                        ProfileCollection.PresentationAddresses.fromProfile(selected));

        assertEquals(1, migrated.networks.size());
        assertEquals(migrated.selectedNetworkId, migrated.selected().id);
        assertTrue(migrated.selected().enabled);
        assertEquals(legacy, migrated.selected().configJson);
        assertPresentation(migrated, IPV4, IPV6);
    }

    @Test
    public void schemaV1MigrationPreservesNetworksExactly() throws Exception {
        String firstConfig = " {\n  \"network\": {\"name\": \"alpha\"}\n} ";
        String secondConfig = "{\"network\":{\"name\":\"beta\"}}";
        ProfileCollection.Decoded decoded =
                ProfileCollection.decode(schemaV1Json(firstConfig, secondConfig));
        AndroidProfile selected = profile(secondConfig, "beta", IPV4, "fd42:0:0:0:0:0:0:9");

        assertEquals(ProfileCollection.Decoded.State.SCHEMA_V1, decoded.state);
        ProfileCollection.SchemaV1Collection schemaV1 = decoded.schemaV1Collection();
        ProfileCollection migrated =
                schemaV1.migrate(
                        ProfileCollection.PresentationAddresses.fromProfile(selected));

        assertEquals(SECOND_ID, migrated.selectedNetworkId);
        assertEquals(2, migrated.networks.size());
        assertEquals(FIRST_ID, migrated.networks.get(0).id);
        assertTrue(migrated.networks.get(0).enabled);
        assertEquals(firstConfig, migrated.networks.get(0).configJson);
        assertEquals(SECOND_ID, migrated.networks.get(1).id);
        assertFalse(migrated.networks.get(1).enabled);
        assertEquals(secondConfig, migrated.networks.get(1).configJson);
        assertPresentation(migrated, IPV4, IPV6);

        JSONObject encoded = new JSONObject(migrated.toJson());
        assertEquals(ProfileCollection.SCHEMA_VERSION, encoded.getInt("schema_version"));
        assertEquals(SECOND_ID, encoded.getString("selected_network_id"));
        assertEquals(
                firstConfig,
                encoded.getJSONArray("networks").getJSONObject(0).getString("config_json"));
        assertEquals(
                secondConfig,
                encoded.getJSONArray("networks").getJSONObject(1).getString("config_json"));
    }

    @Test
    public void schemaV2RoundTripsPresentationSelectionOrderAndEnabledState() throws Exception {
        ProfileCollection collection = collectionWithTwoNetworks();

        ProfileCollection.Decoded decoded = ProfileCollection.decode(collection.toJson());
        ProfileCollection roundTripped = decoded.currentCollection();

        assertEquals(ProfileCollection.Decoded.State.CURRENT, decoded.state);
        assertFalse(decoded.needsMigration());
        assertEquals(SECOND_ID, roundTripped.selectedNetworkId);
        assertEquals(2, roundTripped.networks.size());
        assertEquals(FIRST_ID, roundTripped.networks.get(0).id);
        assertTrue(roundTripped.networks.get(0).enabled);
        assertEquals(SECOND_ID, roundTripped.networks.get(1).id);
        assertFalse(roundTripped.networks.get(1).enabled);
        assertEquals(config("beta"), roundTripped.networks.get(1).configJson);
        assertPresentation(roundTripped, IPV4, IPV6);

        JSONObject addresses =
                new JSONObject(roundTripped.toJson()).getJSONObject("presentation_addresses");
        assertEquals(
                ProfileCollection.PresentationAddresses.IPV4_PREFIX_LENGTH,
                addresses.getJSONObject("ipv4").getInt("prefix_length"));
        assertEquals(
                ProfileCollection.PresentationAddresses.IPV6_PREFIX_LENGTH,
                addresses.getJSONObject("ipv6").getInt("prefix_length"));
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
    public void presentationAddressesRemainStableThroughMutations() throws Exception {
        ProfileCollection original = collectionWithTwoNetworks();
        ProfileCollection.PresentationAddresses addresses = original.presentationAddresses;

        ProfileCollection disabled = original.replace(original.selected().withEnabled(true));
        ProfileCollection renamed =
                disabled.replace(disabled.selected().withConfig(config("renamed")));
        ProfileCollection selected = renamed.select(FIRST_ID);
        ProfileCollection removed = selected.remove(SECOND_ID);
        ProfileCollection added = removed.add(entry(SECOND_ID, false, "restored"), true);

        assertSame(addresses, disabled.presentationAddresses);
        assertSame(addresses, renamed.presentationAddresses);
        assertSame(addresses, selected.presentationAddresses);
        assertSame(addresses, removed.presentationAddresses);
        assertSame(addresses, added.presentationAddresses);
        assertEquals(config("beta"), original.selected().configJson);
        assertEquals(FIRST_ID, removed.selectedNetworkId);
        assertPresentation(added, IPV4, IPV6);
    }

    @Test
    public void malformedMissingAndWrongPrefixV2AddressesFailClosed() throws Exception {
        JSONObject missing = validV2Json();
        missing.getJSONObject("presentation_addresses").remove("ipv6");
        JSONObject malformed = validV2Json();
        malformed.getJSONObject("presentation_addresses").put("ipv4", "10.42.0.9");
        JSONObject wrongPrefix = validV2Json();
        wrongPrefix
                .getJSONObject("presentation_addresses")
                .getJSONObject("ipv4")
                .put("prefix_length", 24);

        assertThrows(P2pVpnException.class, () -> ProfileCollection.decode(missing.toString()));
        assertThrows(P2pVpnException.class, () -> ProfileCollection.decode(malformed.toString()));
        assertThrows(P2pVpnException.class, () -> ProfileCollection.decode(wrongPrefix.toString()));
    }

    @Test
    public void wrongFamilyAndHostnamesFailClosed() throws Exception {
        JSONObject wrongIpv4 = validV2Json();
        setAddress(wrongIpv4, "ipv4", "fd42::9");
        JSONObject wrongIpv6 = validV2Json();
        setAddress(wrongIpv6, "ipv6", "10.42.0.9");
        JSONObject hostname = validV2Json();
        setAddress(hostname, "ipv4", "localhost");

        assertThrows(P2pVpnException.class, () -> ProfileCollection.decode(wrongIpv4.toString()));
        assertThrows(P2pVpnException.class, () -> ProfileCollection.decode(wrongIpv6.toString()));
        assertThrows(P2pVpnException.class, () -> ProfileCollection.decode(hostname.toString()));
    }

    @Test
    public void noncanonicalAddressesFailClosed() throws Exception {
        JSONObject ipv4 = validV2Json();
        setAddress(ipv4, "ipv4", "10.042.0.9");
        JSONObject uppercaseIpv6 = validV2Json();
        setAddress(uppercaseIpv6, "ipv6", "FD42::9");
        JSONObject expandedIpv6 = validV2Json();
        setAddress(expandedIpv6, "ipv6", "fd42:0:0:0:0:0:0:9");

        assertThrows(P2pVpnException.class, () -> ProfileCollection.decode(ipv4.toString()));
        assertThrows(
                P2pVpnException.class, () -> ProfileCollection.decode(uppercaseIpv6.toString()));
        assertThrows(
                P2pVpnException.class, () -> ProfileCollection.decode(expandedIpv6.toString()));
    }

    @Test
    public void duplicateAndUnknownIdentifiersAreRejected() throws Exception {
        ProfileCollection collection =
                ProfileCollection.single(entry(FIRST_ID, true, "alpha"), addresses());

        assertThrows(
                P2pVpnException.class,
                () -> collection.add(entry(FIRST_ID, false, "duplicate"), false));
        assertThrows(P2pVpnException.class, () -> collection.select(SECOND_ID));
        assertThrows(P2pVpnException.class, () -> collection.replace(entry(SECOND_ID, true, "x")));
    }

    @Test
    public void collectionCountAndConfigSizeAreBounded() throws Exception {
        ProfileCollection collection =
                ProfileCollection.single(entry(FIRST_ID, true, "alpha"), addresses());
        for (int index = 1; index < ProfileCollection.MAX_NETWORKS; index++) {
            String id = String.format("%08x-0000-4000-8000-%012x", index, index);
            collection = collection.add(entry(id, true, "network-" + index), false);
        }
        ProfileCollection full = collection;

        assertThrows(
                P2pVpnException.class,
                () ->
                        full.add(
                                entry("ffffffff-ffff-4fff-8fff-ffffffffffff", true, "extra"),
                                false));
        assertThrows(
                P2pVpnException.class,
                () ->
                        new ProfileCollection.Entry(
                                FIRST_ID,
                                true,
                                "x".repeat(ProfileCollection.MAX_CONFIG_BYTES + 1)));
    }

    @Test
    public void unsupportedFutureSchemaDoesNotFallBackToLegacy() throws Exception {
        JSONObject future = validV2Json();
        future.put("schema_version", ProfileCollection.SCHEMA_VERSION + 1);

        assertThrows(P2pVpnException.class, () -> ProfileCollection.decode(future.toString()));
    }

    private static ProfileCollection collectionWithTwoNetworks() throws Exception {
        return ProfileCollection.single(entry(FIRST_ID, true, "alpha"), addresses())
                .add(entry(SECOND_ID, false, "beta"), true);
    }

    private static ProfileCollection.PresentationAddresses addresses() throws Exception {
        return ProfileCollection.PresentationAddresses.of(IPV4, IPV6);
    }

    private static void assertPresentation(
            ProfileCollection collection, String ipv4, String ipv6) {
        assertEquals(ipv4, collection.presentationAddresses.ipv4Address);
        assertEquals(ipv6, collection.presentationAddresses.ipv6Address);
    }

    private static JSONObject validV2Json() throws Exception {
        return new JSONObject(collectionWithTwoNetworks().toJson());
    }

    private static void setAddress(JSONObject collection, String family, String address)
            throws Exception {
        collection
                .getJSONObject("presentation_addresses")
                .getJSONObject(family)
                .put("address", address);
    }

    private static String schemaV1Json(String firstConfig, String secondConfig) throws Exception {
        JSONObject value = new JSONObject();
        value.put("kind", ProfileCollection.KIND);
        value.put("schema_version", 1);
        value.put("selected_network_id", SECOND_ID);
        JSONArray networks = new JSONArray();
        networks.put(encodedEntry(FIRST_ID, true, firstConfig));
        networks.put(encodedEntry(SECOND_ID, false, secondConfig));
        value.put("networks", networks);
        return value.toString();
    }

    private static JSONObject encodedEntry(String id, boolean enabled, String configJson)
            throws Exception {
        JSONObject value = new JSONObject();
        value.put("id", id);
        value.put("enabled", enabled);
        value.put("config_json", configJson);
        return value;
    }

    private static AndroidProfile profile(
            String configJson, String networkName, String ipv4, String ipv6) throws Exception {
        JSONObject value = new JSONObject();
        value.put("config_json", configJson);
        value.put("network_name", networkName);
        value.put("hostname", "android-device");
        value.put("peer_id", "12D3KooWLegacyPeer");
        value.put("interface_name", "pv0");
        value.put("mtu", 1280);
        JSONArray profileAddresses = new JSONArray();
        profileAddresses.put(cidr(ipv6, 128));
        profileAddresses.put(cidr(ipv4, 32));
        value.put("addresses", profileAddresses);
        JSONArray routes = new JSONArray();
        routes.put(cidr("0.0.0.0", 0));
        routes.put(cidr("::", 0));
        value.put("routes", routes);
        return AndroidProfile.fromNative(value);
    }

    private static JSONObject cidr(String address, int prefixLength) throws Exception {
        JSONObject value = new JSONObject();
        value.put("address", address);
        value.put("prefix_length", prefixLength);
        return value;
    }

    private static ProfileCollection.Entry entry(
            String id, boolean enabled, String networkName) throws Exception {
        return new ProfileCollection.Entry(id, enabled, config(networkName));
    }

    private static String config(String networkName) {
        return "{\"network\":{\"name\":\"" + networkName + "\"}}";
    }
}
