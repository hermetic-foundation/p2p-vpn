package org.hermeticfoundation.p2pvpn;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.nio.charset.StandardCharsets;
import java.util.regex.Pattern;

/** Bounded numeric-only view of cached native status for local resource reviews. */
final class DebugResourceSnapshot {
    private static final Pattern COUNTER =
            Pattern.compile(
                    "(?:queue|path|packet_plane|kad|kademlia|public_routing|outbound|inbound|stream|dial|relay|autonat|recovery|membership)_[a-z0-9_]+ [0-9]{1,20}");

    private DebugResourceSnapshot() {}

    static JSONObject from(JSONObject status) throws JSONException {
        JSONObject result = new JSONObject();
        result.put("phase", phase(status));
        result.put("counters", counters(status));
        JSONArray networks =
                status.has("networks") ? status.getJSONArray("networks") : new JSONArray();
        if (networks.length() > ProfileCollection.MAX_NETWORKS) {
            throw new JSONException("Too many resource networks");
        }
        JSONArray selected = new JSONArray();
        for (int index = 0; index < networks.length(); index++) {
            JSONObject network = networks.getJSONObject(index);
            String id = network.getString("id");
            if (!id.matches("[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}")) {
                throw new JSONException("Invalid resource network ID");
            }
            JSONObject entry = new JSONObject();
            entry.put("id", id);
            entry.put("phase", phase(network));
            entry.put("counters", counters(network));
            selected.put(entry);
        }
        result.put("networks", selected);
        if (result.toString().getBytes(StandardCharsets.UTF_8).length > 64 * 1024) {
            throw new JSONException("Resource response exceeds byte budget");
        }
        return result;
    }

    private static String phase(JSONObject value) throws JSONException {
        String phase = value.getString("phase");
        if (!phase.matches("starting|running|stopped|failed")) {
            throw new JSONException("Invalid resource phase");
        }
        return phase;
    }

    private static JSONArray counters(JSONObject value) throws JSONException {
        JSONArray lines = value.getJSONArray("lines");
        if (lines.length() > 4096) {
            throw new JSONException("Resource lines exceed count budget");
        }
        JSONArray result = new JSONArray();
        for (int index = 0; index < lines.length(); index++) {
            String line = lines.getString(index);
            if (line.length() <= 160 && COUNTER.matcher(line.trim()).matches()) {
                result.put(line.trim());
            }
        }
        return result;
    }
}
