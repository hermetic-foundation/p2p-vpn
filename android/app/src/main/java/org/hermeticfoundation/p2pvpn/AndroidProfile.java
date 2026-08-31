package org.hermeticfoundation.p2pvpn;

import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.UnknownHostException;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

final class AndroidProfile {
    final String configJson;
    final String networkName;
    final String hostname;
    final String peerId;
    final String interfaceName;
    final int mtu;
    final List<Cidr> addresses;
    final List<Cidr> routes;

    private AndroidProfile(
            String configJson,
            String networkName,
            String hostname,
            String peerId,
            String interfaceName,
            int mtu,
            List<Cidr> addresses,
            List<Cidr> routes) {
        this.configJson = configJson;
        this.networkName = networkName;
        this.hostname = hostname;
        this.peerId = peerId;
        this.interfaceName = interfaceName;
        this.mtu = mtu;
        this.addresses = Collections.unmodifiableList(new ArrayList<>(addresses));
        this.routes = Collections.unmodifiableList(new ArrayList<>(routes));
    }

    static AndroidProfile fromNative(JSONObject value) throws P2pVpnException {
        try {
            String configJson = requiredString(value, "config_json");
            String networkName = requiredString(value, "network_name");
            String hostname = requiredString(value, "hostname");
            String peerId = requiredString(value, "peer_id");
            String interfaceName = requiredString(value, "interface_name");
            int mtu = value.getInt("mtu");
            if (mtu < 576 || mtu > 65_535) {
                throw new P2pVpnException("Native profile contains an invalid MTU");
            }
            List<Cidr> addresses = parseCidrs(value.getJSONArray("addresses"));
            List<Cidr> routes = parseCidrs(value.getJSONArray("routes"));
            if (addresses.isEmpty() || routes.isEmpty()) {
                throw new P2pVpnException(
                        "Native profile does not contain overlay addresses and routes");
            }
            return new AndroidProfile(
                    configJson,
                    networkName,
                    hostname,
                    peerId,
                    interfaceName,
                    mtu,
                    addresses,
                    routes);
        } catch (JSONException error) {
            throw new P2pVpnException("Native profile is malformed", error);
        }
    }

    private static List<Cidr> parseCidrs(JSONArray values)
            throws JSONException, P2pVpnException {
        List<Cidr> cidrs = new ArrayList<>(values.length());
        for (int index = 0; index < values.length(); index++) {
            JSONObject value = values.getJSONObject(index);
            String address = requiredString(value, "address");
            int prefixLength = value.getInt("prefix_length");
            cidrs.add(Cidr.parse(address, prefixLength));
        }
        return cidrs;
    }

    private static String requiredString(JSONObject value, String key)
            throws JSONException, P2pVpnException {
        String result = value.getString(key);
        if (result.isEmpty()) {
            throw new P2pVpnException("Native profile field is empty: " + key);
        }
        return result;
    }

    static final class Cidr {
        final String address;
        final int prefixLength;
        final InetAddress inetAddress;

        private Cidr(String address, int prefixLength, InetAddress inetAddress) {
            this.address = address;
            this.prefixLength = prefixLength;
            this.inetAddress = inetAddress;
        }

        static Cidr parse(String address, int prefixLength) throws P2pVpnException {
            try {
                InetAddress parsed = InetAddress.getByName(address);
                int maximum = parsed instanceof Inet4Address ? 32 : 128;
                if (prefixLength < 0 || prefixLength > maximum) {
                    throw new P2pVpnException("Native profile contains an invalid prefix length");
                }
                return new Cidr(address, prefixLength, parsed);
            } catch (UnknownHostException error) {
                throw new P2pVpnException("Native profile contains an invalid IP address", error);
            }
        }
    }
}
