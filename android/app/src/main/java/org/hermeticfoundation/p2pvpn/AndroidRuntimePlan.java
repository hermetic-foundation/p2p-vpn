package org.hermeticfoundation.p2pvpn;

import java.io.File;
import java.net.Inet4Address;
import java.net.Inet6Address;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

final class AndroidRuntimePlan {
    private static final int SCHEMA_VERSION = 1;
    private static final int MAX_REQUEST_BYTES = 8 * 1024 * 1024;

    final String requestJson;
    final String sessionName;
    final int mtu;
    final List<String> networkIds;
    final List<AndroidProfile.Cidr> addresses;
    final List<AndroidProfile.Cidr> routes;

    private AndroidRuntimePlan(
            String requestJson,
            String sessionName,
            int mtu,
            List<String> networkIds,
            List<AndroidProfile.Cidr> addresses,
            List<AndroidProfile.Cidr> routes) {
        this.requestJson = requestJson;
        this.sessionName = sessionName;
        this.mtu = mtu;
        this.networkIds = Collections.unmodifiableList(new ArrayList<>(networkIds));
        this.addresses = Collections.unmodifiableList(new ArrayList<>(addresses));
        this.routes = Collections.unmodifiableList(new ArrayList<>(routes));
    }

    static AndroidRuntimePlan create(
            ProfileCollection collection,
            Map<String, AndroidProfile> profiles,
            Map<String, StatePaths> statePaths)
            throws P2pVpnException {
        if (collection == null || profiles == null || statePaths == null) {
            throw new P2pVpnException("Cannot plan an incomplete Android runtime");
        }
        if (profiles.size() != collection.networks.size()
                || statePaths.size() != collection.networks.size()) {
            throw new P2pVpnException("Android runtime inputs do not match the profile collection");
        }

        List<String> networkIds = new ArrayList<>();
        List<AndroidProfile> enabledProfiles = new ArrayList<>();
        JSONArray encodedNetworks = new JSONArray();
        int mtu = Integer.MAX_VALUE;
        try {
            for (ProfileCollection.Entry entry : collection.networks) {
                AndroidProfile profile = profiles.get(entry.id);
                StatePaths paths = statePaths.get(entry.id);
                if (profile == null || paths == null) {
                    throw new P2pVpnException(
                            "Android runtime is missing inspected network state");
                }
                paths.validateForNetwork(entry.id);
                if (!entry.enabled) {
                    continue;
                }
                networkIds.add(entry.id);
                enabledProfiles.add(profile);
                mtu = Math.min(mtu, profile.mtu);
                JSONObject encodedNetwork = new JSONObject();
                encodedNetwork.put("id", entry.id);
                encodedNetwork.put("config_json", profile.configJson);
                encodedNetwork.put("pairing_state_path", paths.pairingStatePath);
                encodedNetwork.put("membership_state_path", paths.membershipStatePath);
                encodedNetworks.put(encodedNetwork);
            }
            if (networkIds.isEmpty()) {
                throw new P2pVpnException("Enable at least one network to connect");
            }

            LinkedHashMap<String, AndroidProfile.Cidr> addresses = new LinkedHashMap<>();
            addCidr(
                    addresses,
                    AndroidProfile.Cidr.parse(
                            collection.presentationAddresses.ipv4Address,
                            ProfileCollection.PresentationAddresses.IPV4_PREFIX_LENGTH));
            addCidr(
                    addresses,
                    AndroidProfile.Cidr.parse(
                            collection.presentationAddresses.ipv6Address,
                            ProfileCollection.PresentationAddresses.IPV6_PREFIX_LENGTH));
            LinkedHashMap<String, AndroidProfile.Cidr> routes = new LinkedHashMap<>();
            for (AndroidProfile profile : enabledProfiles) {
                addAdditionalAddresses(addresses, profile);
                for (AndroidProfile.Cidr route : profile.routes) {
                    addCidr(routes, route);
                }
            }

            JSONObject presentation = new JSONObject();
            presentation.put("ipv4", collection.presentationAddresses.ipv4Address);
            presentation.put("ipv6", collection.presentationAddresses.ipv6Address);
            JSONObject request = new JSONObject();
            request.put("schema_version", SCHEMA_VERSION);
            request.put("presentation_addresses", presentation);
            request.put("networks", encodedNetworks);
            String requestJson = request.toString();
            int requestBytes = requestJson.getBytes(StandardCharsets.UTF_8).length;
            if (requestBytes == 0 || requestBytes > MAX_REQUEST_BYTES) {
                throw new P2pVpnException("Android runtime start request has an invalid size");
            }
            String sessionName =
                    enabledProfiles.size() == 1
                            ? enabledProfiles.get(0).networkName
                            : "p2p-vpn (" + enabledProfiles.size() + " networks)";
            return new AndroidRuntimePlan(
                    requestJson,
                    sessionName,
                    mtu,
                    networkIds,
                    new ArrayList<>(addresses.values()),
                    new ArrayList<>(routes.values()));
        } catch (JSONException error) {
            throw new P2pVpnException("Failed to encode Android runtime start request", error);
        }
    }

    private static void addAdditionalAddresses(
            Map<String, AndroidProfile.Cidr> addresses, AndroidProfile profile)
            throws P2pVpnException {
        boolean primaryIpv4Skipped = false;
        boolean primaryIpv6Skipped = false;
        for (AndroidProfile.Cidr address : profile.addresses) {
            if (!primaryIpv4Skipped && address.inetAddress instanceof Inet4Address) {
                primaryIpv4Skipped = true;
                continue;
            }
            if (!primaryIpv6Skipped && address.inetAddress instanceof Inet6Address) {
                primaryIpv6Skipped = true;
                continue;
            }
            addCidr(addresses, address);
        }
        if (!primaryIpv4Skipped || !primaryIpv6Skipped) {
            throw new P2pVpnException(
                    "Enabled Android profile is missing primary IPv4 or IPv6 identity");
        }
    }

    private static void addCidr(
            Map<String, AndroidProfile.Cidr> values, AndroidProfile.Cidr cidr) {
        String key = cidr.inetAddress.getHostAddress() + "/" + cidr.prefixLength;
        values.putIfAbsent(key, cidr);
    }

    static final class StatePaths {
        final String directoryPath;
        final String pairingStatePath;
        final String membershipStatePath;

        StatePaths(String directoryPath, String pairingStatePath, String membershipStatePath)
                throws P2pVpnException {
            this.directoryPath = requireAbsolutePath(directoryPath, "runtime directory");
            this.pairingStatePath = requireAbsolutePath(pairingStatePath, "pairing state");
            this.membershipStatePath = requireAbsolutePath(membershipStatePath, "membership state");
        }

        private void validateForNetwork(String networkId) throws P2pVpnException {
            File directory = new File(directoryPath);
            File pairingState = new File(pairingStatePath);
            File membershipState = new File(membershipStatePath);
            if (!networkId.equals(directory.getName())
                    || !directory.equals(pairingState.getParentFile())
                    || !directory.equals(membershipState.getParentFile())
                    || !"pairing-state.json".equals(pairingState.getName())
                    || !"membership-state.json".equals(membershipState.getName())) {
                throw new P2pVpnException("Android runtime state does not match its network");
            }
        }

        private static String requireAbsolutePath(String value, String label)
                throws P2pVpnException {
            if (value == null || value.trim().isEmpty() || !new File(value).isAbsolute()) {
                throw new P2pVpnException("Android " + label + " path is invalid");
            }
            return value;
        }
    }
}
