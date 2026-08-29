package org.hermeticfoundation.p2pvpn;

import android.content.Context;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.AtomicFile;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.security.KeyStore;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

final class ProfileStore {
    private static final int PROFILE_MAGIC = 0x50325650;
    private static final int PAIRING_MAGIC = 0x50325041;
    private static final int VERSION = 1;
    private static final int MAX_PROFILE_BYTES = 2 * 1024 * 1024;
    private static final int MAX_PAIRING_BYTES = 16 * 1024;
    private static final String KEYSTORE = "AndroidKeyStore";
    private static final String KEY_ALIAS = "org.hermeticfoundation.p2pvpn.profile.v1";
    private static final String CIPHER = "AES/GCM/NoPadding";
    private static final byte[] PROFILE_AAD =
            "org.hermeticfoundation.p2pvpn/profile/v1".getBytes(StandardCharsets.UTF_8);
    private static final byte[] PAIRING_AAD =
            "org.hermeticfoundation.p2pvpn/pairing/v1".getBytes(StandardCharsets.UTF_8);

    private final AtomicFile profileFile;
    private final AtomicFile pairingFile;

    ProfileStore(Context context) {
        File directory = context.getNoBackupFilesDir();
        profileFile = new AtomicFile(new File(directory, "profile.enc"));
        pairingFile = new AtomicFile(new File(directory, "pairing-operation.enc"));
    }

    synchronized boolean exists() {
        return profileFile.getBaseFile().isFile();
    }

    synchronized void save(String configJson) throws P2pVpnException {
        saveEncrypted(
                profileFile,
                PROFILE_MAGIC,
                PROFILE_AAD,
                configJson,
                MAX_PROFILE_BYTES,
                "profile");
    }

    synchronized String load() throws P2pVpnException {
        if (!exists()) {
            throw new P2pVpnException("No p2p-vpn profile has been created");
        }
        return loadEncrypted(
                profileFile,
                PROFILE_MAGIC,
                PROFILE_AAD,
                MAX_PROFILE_BYTES,
                "stored profile");
    }

    synchronized boolean pairingExists() {
        return pairingFile.getBaseFile().isFile();
    }

    synchronized void savePairing(String metadataJson) throws P2pVpnException {
        saveEncrypted(
                pairingFile,
                PAIRING_MAGIC,
                PAIRING_AAD,
                metadataJson,
                MAX_PAIRING_BYTES,
                "pairing operation");
    }

    synchronized String loadPairing() throws P2pVpnException {
        if (!pairingExists()) {
            throw new P2pVpnException("No pairing operation has been saved");
        }
        return loadEncrypted(
                pairingFile,
                PAIRING_MAGIC,
                PAIRING_AAD,
                MAX_PAIRING_BYTES,
                "saved pairing operation");
    }

    synchronized void clearPairing() {
        pairingFile.delete();
    }

    private static void saveEncrypted(
            AtomicFile target,
            int magic,
            byte[] aad,
            String value,
            int maximumBytes,
            String label)
            throws P2pVpnException {
        byte[] plaintext = value.getBytes(StandardCharsets.UTF_8);
        if (plaintext.length == 0 || plaintext.length > maximumBytes) {
            throw new P2pVpnException("Invalid " + label + " size");
        }
        try {
            Cipher cipher = Cipher.getInstance(CIPHER);
            cipher.init(Cipher.ENCRYPT_MODE, profileKey());
            cipher.updateAAD(aad);
            byte[] ciphertext = cipher.doFinal(plaintext);
            byte[] iv = cipher.getIV();

            FileOutputStream output = null;
            try {
                output = target.startWrite();
                DataOutputStream data = new DataOutputStream(output);
                data.writeInt(magic);
                data.writeInt(VERSION);
                data.writeInt(iv.length);
                data.writeInt(ciphertext.length);
                data.write(iv);
                data.write(ciphertext);
                data.flush();
                target.finishWrite(output);
            } catch (IOException error) {
                if (output != null) {
                    target.failWrite(output);
                }
                throw error;
            }
        } catch (GeneralSecurityException | IOException error) {
            throw new P2pVpnException("Failed to encrypt and persist " + label, error);
        }
    }

    private static String loadEncrypted(
            AtomicFile source,
            int magic,
            byte[] aad,
            int maximumBytes,
            String label)
            throws P2pVpnException {
        try (DataInputStream input = new DataInputStream(source.openRead())) {
            if (input.readInt() != magic || input.readInt() != VERSION) {
                throw new P2pVpnException(label + " has an unsupported format");
            }
            int ivLength = input.readInt();
            int ciphertextLength = input.readInt();
            if (ivLength < 12 || ivLength > 32) {
                throw new P2pVpnException(label + " has an invalid nonce");
            }
            if (ciphertextLength < 16 || ciphertextLength > maximumBytes + 32) {
                throw new P2pVpnException(label + " has an invalid size");
            }
            byte[] iv = new byte[ivLength];
            byte[] ciphertext = new byte[ciphertextLength];
            input.readFully(iv);
            input.readFully(ciphertext);
            if (input.read() != -1) {
                throw new P2pVpnException(label + " contains trailing data");
            }

            Cipher cipher = Cipher.getInstance(CIPHER);
            cipher.init(Cipher.DECRYPT_MODE, profileKey(), new GCMParameterSpec(128, iv));
            cipher.updateAAD(aad);
            byte[] plaintext = cipher.doFinal(ciphertext);
            return new String(plaintext, StandardCharsets.UTF_8);
        } catch (P2pVpnException error) {
            throw error;
        } catch (GeneralSecurityException | IOException error) {
            throw new P2pVpnException("Failed to decrypt " + label, error);
        }
    }

    private static SecretKey profileKey() throws GeneralSecurityException, IOException {
        KeyStore keyStore = KeyStore.getInstance(KEYSTORE);
        keyStore.load(null);
        if (keyStore.containsAlias(KEY_ALIAS)) {
            return (SecretKey) keyStore.getKey(KEY_ALIAS, null);
        }
        KeyGenerator generator =
                KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE);
        generator.init(
                new KeyGenParameterSpec.Builder(
                                KEY_ALIAS,
                                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                        .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                        .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                        .setRandomizedEncryptionRequired(true)
                        .build());
        return generator.generateKey();
    }
}
