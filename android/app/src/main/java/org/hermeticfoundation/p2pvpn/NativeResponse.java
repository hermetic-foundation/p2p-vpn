package org.hermeticfoundation.p2pvpn;

import org.json.JSONException;
import org.json.JSONObject;

final class NativeResponse {
    private NativeResponse() {}

    static JSONObject objectValue(String encoded) throws P2pVpnException {
        try {
            JSONObject envelope = new JSONObject(encoded);
            if (!envelope.optBoolean("ok", false)) {
                throw new P2pVpnException(envelope.optString("error", "Native operation failed"));
            }
            Object value = envelope.opt("value");
            if (!(value instanceof JSONObject)) {
                throw new P2pVpnException("Native operation returned an invalid response");
            }
            return (JSONObject) value;
        } catch (JSONException | NullPointerException error) {
            throw new P2pVpnException("Native operation returned malformed JSON", error);
        }
    }
}
