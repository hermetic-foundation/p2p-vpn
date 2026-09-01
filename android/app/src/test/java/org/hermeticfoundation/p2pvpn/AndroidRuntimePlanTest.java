package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import java.util.Arrays;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;
import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

public final class AndroidRuntimePlanTest {
    private static final String ALPHA = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    private static final String BETA = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    @Test
    public void plansEnabledNetworksInOrderOverOneSharedTun() throws Exception {
        AndroidProfile alpha = profile("alpha", "peer-alpha", 1400, "192.0.2.10", "10.60.0.0");
        AndroidProfile beta = profile("beta", "peer-beta", 1280, "192.0.2.11", "10.61.0.0");
        ProfileCollection collection = collection(true, alpha, true, beta);

        AndroidRuntimePlan plan =
                AndroidRuntimePlan.create(
                        collection, profiles(alpha, beta), statePaths(ALPHA, BETA));
        JSONObject request = new JSONObject(plan.requestJson);
        JSONArray networks = request.getJSONArray("networks");

        assertEquals(1, request.getInt("schema_version"));
        assertEquals("10.42.0.9", request.getJSONObject("presentation_addresses").getString("ipv4"));
        assertEquals(ALPHA, networks.getJSONObject(0).getString("id"));
        assertEquals(BETA, networks.getJSONObject(1).getString("id"));
        assertEquals(Arrays.asList(ALPHA, BETA), plan.networkIds);
        assertEquals(1280, plan.mtu);
        assertEquals("p2p-vpn (2 networks)", plan.sessionName);
        assertEquals(4, plan.addresses.size());
        assertEquals("10.42.0.9", plan.addresses.get(0).address);
        assertEquals("fd42::9", plan.addresses.get(1).address);
        assertTrue(hasCidr(plan.addresses, "192.0.2.10", 32));
        assertTrue(hasCidr(plan.addresses, "192.0.2.11", 32));
        assertEquals(4, plan.routes.size());
    }

    @Test
    public void leavesDisabledNetworksDormant() throws Exception {
        AndroidProfile alpha = profile("alpha", "peer-alpha", 1400, "192.0.2.10", "10.60.0.0");
        AndroidProfile beta = profile("beta", "peer-beta", 1280, "192.0.2.11", "10.61.0.0");
        ProfileCollection collection = collection(true, alpha, false, beta);

        AndroidRuntimePlan plan =
                AndroidRuntimePlan.create(
                        collection, profiles(alpha, beta), statePaths(ALPHA, BETA));

        assertEquals(Collections.singletonList(ALPHA), plan.networkIds);
        assertEquals("alpha", plan.sessionName);
        assertFalse(hasCidr(plan.addresses, "192.0.2.11", 32));
        assertFalse(plan.requestJson.contains("peer-beta"));
    }

    @Test
    public void rejectsZeroEnabledOrIncompleteInputs() throws Exception {
        AndroidProfile alpha = profile("alpha", "peer-alpha", 1400, "192.0.2.10", "10.60.0.0");
        AndroidProfile beta = profile("beta", "peer-beta", 1280, "192.0.2.11", "10.61.0.0");
        ProfileCollection disabled = collection(false, alpha, false, beta);

        assertThrows(
                P2pVpnException.class,
                () ->
                        AndroidRuntimePlan.create(
                                disabled, profiles(alpha, beta), statePaths(ALPHA, BETA)));

        Map<String, AndroidProfile> missingProfile = new LinkedHashMap<>();
        missingProfile.put(ALPHA, alpha);
        assertThrows(
                P2pVpnException.class,
                () ->
                        AndroidRuntimePlan.create(
                                collection(true, alpha, true, beta),
                                missingProfile,
                                statePaths(ALPHA, BETA)));
    }

    @Test
    public void exposesImmutablePlanCollections() throws Exception {
        AndroidProfile alpha = profile("alpha", "peer-alpha", 1400, "192.0.2.10", "10.60.0.0");
        AndroidProfile beta = profile("beta", "peer-beta", 1280, "192.0.2.11", "10.61.0.0");
        AndroidRuntimePlan plan =
                AndroidRuntimePlan.create(
                        collection(true, alpha, true, beta),
                        profiles(alpha, beta),
                        statePaths(ALPHA, BETA));

        assertThrows(UnsupportedOperationException.class, () -> plan.networkIds.add(ALPHA));
        assertThrows(UnsupportedOperationException.class, () -> plan.addresses.clear());
        assertThrows(UnsupportedOperationException.class, () -> plan.routes.clear());
    }

    private static ProfileCollection collection(
            boolean alphaEnabled,
            AndroidProfile alpha,
            boolean betaEnabled,
            AndroidProfile beta)
            throws Exception {
        ProfileCollection collection =
                ProfileCollection.single(
                        new ProfileCollection.Entry(ALPHA, alphaEnabled, alpha.configJson),
                        ProfileCollection.PresentationAddresses.of("10.42.0.9", "fd42::9"));
        return collection.add(
                new ProfileCollection.Entry(BETA, betaEnabled, beta.configJson), false);
    }

    private static Map<String, AndroidProfile> profiles(
            AndroidProfile alpha, AndroidProfile beta) {
        Map<String, AndroidProfile> profiles = new LinkedHashMap<>();
        profiles.put(ALPHA, alpha);
        profiles.put(BETA, beta);
        return profiles;
    }

    private static Map<String, AndroidRuntimePlan.StatePaths> statePaths(String... ids)
            throws Exception {
        Map<String, AndroidRuntimePlan.StatePaths> paths = new LinkedHashMap<>();
        for (String id : ids) {
            String directory = "/data/user/0/org.hermeticfoundation.p2pvpn/no_backup/runtime/" + id;
            paths.put(
                    id,
                    new AndroidRuntimePlan.StatePaths(
                            directory,
                            directory + "/pairing-state.json",
                            directory + "/membership-state.json"));
        }
        return paths;
    }

    private static AndroidProfile profile(
            String network, String peer, int mtu, String additionalAddress, String uniqueRoute)
            throws Exception {
        JSONObject value = new JSONObject();
        value.put("config_json", "{\"peer\":" + JSONObject.quote(peer) + "}");
        value.put("network_name", network);
        value.put("hostname", network + "-device");
        value.put("peer_id", peer);
        value.put("interface_name", "pv0");
        value.put("mtu", mtu);
        value.put(
                "addresses",
                new JSONArray()
                        .put(cidr("10.42.0." + ("alpha".equals(network) ? "1" : "2"), 32))
                        .put(cidr("fd42::" + ("alpha".equals(network) ? "1" : "2"), 128))
                        .put(cidr(additionalAddress, 32)));
        value.put(
                "routes",
                new JSONArray()
                        .put(cidr("10.42.0.0", 16))
                        .put(cidr("fd42::", 64))
                        .put(cidr(uniqueRoute, 16)));
        return AndroidProfile.fromNative(value);
    }

    private static JSONObject cidr(String address, int prefixLength) throws Exception {
        return new JSONObject().put("address", address).put("prefix_length", prefixLength);
    }

    private static boolean hasCidr(
            Iterable<AndroidProfile.Cidr> cidrs, String address, int prefixLength) {
        for (AndroidProfile.Cidr cidr : cidrs) {
            if (address.equals(cidr.address) && prefixLength == cidr.prefixLength) {
                return true;
            }
        }
        return false;
    }
}
