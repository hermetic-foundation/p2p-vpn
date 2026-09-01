package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

public final class NetworkSnapshotTest {
    private static final String ALPHA = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    private static final String BETA = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    private static final String GAMMA = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

    @Test
    public void exposesSelectedEnabledAndIndependentRuntimeState() throws Exception {
        Map<String, AndroidProfile> profiles = profiles();
        ProfileCollection collection = collection(profiles);
        Map<String, P2pVpnService.NetworkRuntimeStatus> statuses = new LinkedHashMap<>();
        statuses.put(ALPHA, runtimeStatus(ALPHA, "running", ""));

        List<P2pVpnService.NetworkSnapshot> snapshots =
                P2pVpnService.networkSnapshots(
                        collection, profiles, statuses, true, true, "Connected");

        assertEquals(3, snapshots.size());
        assertEquals("alpha", snapshots.get(0).name);
        assertTrue(snapshots.get(0).selected);
        assertTrue(snapshots.get(0).enabled);
        assertEquals("running", snapshots.get(0).phase);
        assertFalse(snapshots.get(1).selected);
        assertEquals("starting", snapshots.get(1).phase);
        assertEquals("Waiting for runtime status", snapshots.get(1).detail);
        assertFalse(snapshots.get(2).enabled);
        assertEquals("disabled", snapshots.get(2).phase);
        assertThrows(UnsupportedOperationException.class, snapshots::clear);
        assertThrows(UnsupportedOperationException.class, snapshots.get(0).addresses::clear);
    }

    @Test
    public void enabledNetworksReflectRequestedAndStoppedLifecycle() throws Exception {
        Map<String, AndroidProfile> profiles = profiles();
        ProfileCollection collection = collection(profiles);

        List<P2pVpnService.NetworkSnapshot> connecting =
                P2pVpnService.networkSnapshots(
                        collection,
                        profiles,
                        Collections.emptyMap(),
                        true,
                        false,
                        "Reconnecting");
        List<P2pVpnService.NetworkSnapshot> stopped =
                P2pVpnService.networkSnapshots(
                        collection,
                        profiles,
                        Collections.emptyMap(),
                        false,
                        false,
                        "Disconnected");

        assertEquals("starting", connecting.get(0).phase);
        assertEquals("Reconnecting", connecting.get(0).detail);
        assertEquals("stopped", stopped.get(0).phase);
        assertEquals("disabled", stopped.get(2).phase);
        assertTrue(
                P2pVpnService.networkSnapshots(
                                null,
                                Collections.emptyMap(),
                                Collections.emptyMap(),
                                false,
                                false,
                                "")
                        .isEmpty());
    }

    private static ProfileCollection collection(Map<String, AndroidProfile> profiles)
            throws Exception {
        ProfileCollection collection =
                ProfileCollection.single(
                        entry(ALPHA, true, profiles.get(ALPHA)),
                        ProfileCollection.PresentationAddresses.of("10.42.0.9", "fd42::9"));
        collection = collection.add(entry(BETA, true, profiles.get(BETA)), false);
        return collection.add(entry(GAMMA, false, profiles.get(GAMMA)), false);
    }

    private static ProfileCollection.Entry entry(
            String id, boolean enabled, AndroidProfile profile) throws Exception {
        return new ProfileCollection.Entry(id, enabled, profile.configJson);
    }

    private static Map<String, AndroidProfile> profiles() throws Exception {
        Map<String, AndroidProfile> profiles = new LinkedHashMap<>();
        profiles.put(ALPHA, profile("alpha", "10.42.0.1", "fd42::1"));
        profiles.put(BETA, profile("beta", "10.42.0.2", "fd42::2"));
        profiles.put(GAMMA, profile("gamma", "10.42.0.3", "fd42::3"));
        return profiles;
    }

    private static AndroidProfile profile(String name, String ipv4, String ipv6)
            throws Exception {
        JSONObject value = new JSONObject();
        value.put("config_json", "{\"network\":" + JSONObject.quote(name) + "}");
        value.put("network_name", name);
        value.put("hostname", name + "-device");
        value.put("peer_id", "peer-" + name);
        value.put("interface_name", "pv0");
        value.put("mtu", 1280);
        value.put(
                "addresses",
                new JSONArray()
                        .put(cidr(ipv4, 32))
                        .put(cidr(ipv6, 128)));
        value.put(
                "routes",
                new JSONArray()
                        .put(cidr("100.64.0.0", 16))
                        .put(cidr("fd42::", 64)));
        return AndroidProfile.fromNative(value);
    }

    private static P2pVpnService.NetworkRuntimeStatus runtimeStatus(
            String id, String phase, String detail) throws Exception {
        JSONObject value = new JSONObject();
        value.put("id", id);
        value.put("phase", phase);
        value.put("detail", detail);
        value.put("lines", new JSONArray());
        return P2pVpnService.NetworkRuntimeStatus.from(value);
    }

    private static JSONObject cidr(String address, int prefixLength) throws Exception {
        return new JSONObject()
                .put("address", address)
                .put("prefix_length", prefixLength);
    }
}
