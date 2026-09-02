package org.hermeticfoundation.p2pvpn;

final class NativeBridge {
    static {
        System.loadLibrary("p2p_vpn_android");
    }

    private NativeBridge() {}

    static native String nativeCreateProfile(String networkName, String hostname);

    static native String nativeCreateE2eProfile(
            String networkName,
            String bootstrapPeerId,
            String bootstrapAddress,
            String kademliaProtocol,
            String packetQuicListen,
            String packetQuicExternalEndpoint,
            String relayReservation,
            String additionalRoute);

    static native String nativeInspectProfile(String configJson);

    static native String nativeRenameProfile(String configJson, String hostname);

    static native String nativeStart(
            String networkId,
            String configJson,
            int tunFd,
            String pairingStatePath,
            String membershipStatePath);

    static native String nativeStartNetworks(String requestJson, int tunFd);

    static native String nativeValidateStartNetworks(String requestJson);

    static native String nativeStop();

    static native String nativeStatus();

    static native String nativeNetworkChanged();

    static native String nativePairRpc(String requestJson);

    static native String nativePairRpcForNetwork(String networkId, String requestJson);

    static native String nativeApplyPairingArtifacts(
            String configJson, String artifactsJson, String runtimeStateDirectory);

    static native String nativeJoinProfileByCode(
            String operationId,
            String pairingCode,
            String hostname,
            String existingNetworkNamesJson,
            String candidateHintsJson);

    static native String nativeCancelProfileJoin(String operationId);
}
