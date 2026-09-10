package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;
import org.junit.Test;

public final class DebugResourceSnapshotTest {
    private static JSONObject status() throws JSONException {
        return new JSONObject().put("phase", "running").put("lines", new JSONArray());
    }

    @Test
    public void retainsOnlyNumericCountersAndNetworkIdentity() throws Exception {
        JSONObject network = status();
        network.put("id", "00000000-0000-0000-0000-000000000001");
        network.put("detail", "private.example");
        network.put("peer_snapshot", new JSONObject().put("peer_id", "secret-peer"));
        network.put(
                "lines",
                new JSONArray()
                        .put("  queue_queued_bytes 12")
                        .put("path_healthy_direct_quic_stream_paths 1")
                        .put("private_key 1234")
                        .put("dial_address /ip4/192.0.2.1/tcp/1")
                        .put("queue_queued_bytes -1")
                        .put("queue_queued_bytes unknown"));
        JSONObject value = status().put("networks", new JSONArray().put(network));
        JSONObject result = DebugResourceSnapshot.from(value);
        JSONArray counters =
                result.getJSONArray("networks").getJSONObject(0).getJSONArray("counters");
        assertEquals(2, counters.length());
        assertEquals("queue_queued_bytes 12", counters.getString(0));
        assertFalse(result.toString().contains("private"));
        assertFalse(result.toString().contains("secret-peer"));
        assertFalse(result.toString().contains("192.0.2.1"));
    }

    @Test
    public void absentNetworksAndMissingCountersStayEmpty() throws Exception {
        JSONObject result = DebugResourceSnapshot.from(status());
        assertEquals(0, result.getJSONArray("networks").length());
        assertEquals(0, result.getJSONArray("counters").length());
    }

    @Test(expected = JSONException.class)
    public void rejectsUnboundedLines() throws Exception {
        JSONArray lines = new JSONArray();
        for (int index = 0; index < 4097; index++) {
            lines.put("queue_queued_bytes 1");
        }
        DebugResourceSnapshot.from(status().put("lines", lines));
    }

    @Test(expected = JSONException.class)
    public void rejectsFreeFormIdentity() throws Exception {
        DebugResourceSnapshot.from(
                status().put("networks", new JSONArray().put(status().put("id", "private.example"))));
    }

    @Test(expected = JSONException.class)
    public void rejectsOversizedResponse() throws Exception {
        JSONArray lines = new JSONArray();
        for (int index = 0; index < 4096; index++) {
            lines.put("queue_queued_bytes 18446744073709551615");
        }
        DebugResourceSnapshot.from(status().put("lines", lines));
    }

    @Test(expected = JSONException.class)
    public void rejectsTooManyNetworks() throws Exception {
        JSONArray networks = new JSONArray();
        for (int index = 0; index <= ProfileCollection.MAX_NETWORKS; index++) {
            networks.put(status());
        }
        DebugResourceSnapshot.from(status().put("networks", networks));
    }

    @Test(expected = JSONException.class)
    public void rejectsMalformedNetworkCollection() throws Exception {
        DebugResourceSnapshot.from(status().put("networks", "not-an-array"));
    }
}
