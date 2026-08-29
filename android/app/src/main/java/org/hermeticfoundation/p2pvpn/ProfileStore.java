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
    private static final int MAGIC = 0x50325650;
    private static final int VERSION = 1;
    private static final int MAX_PROFILE_BYTES = 2 * 1024 * 1024;
    private static final String KEYSTORE = "AndroidKeyStore";
    private static final String KEY_ALIAS = "org.hermeticfoundation.p2pvpn.profile.v1";
    private static final String CIPHER = "AES/GCM/NoPadding";
    private static final byte[] AAD =
            "org.hermeticfoundation.p2pvpn/profile/v1".getBytes(StandardCharsets.UTF_8);

    private final AtomicFile profileFile;

    ProfileStore(Context context) {
        File file = new File(context.getNoBackupFilesDir(), "profile.enc");
        profileFile = new AtomicFile(file);
    }

    synchronized boolean exists() {
        return profileFile.getBaseFile().isFile();
    }

    synchronized void save(String configJson) throws P2pVpnException {
        byte[] plaintext = configJson.getBytes(StandardCharsets.UTF_8);
        if (plaintext.length == 0 || plaintext.length > MAX_PROFILE_BYTES) {
            throw new P2pVpnException("Profile size is invalid");
        }

        try {
            Cipher cipher = Cipher.getInstance(CIPHER);
            cipher.init(Cipher.ENCRYPT_MODE, profileKey());
            cipher.updateAAD(AAD);
            byte[] ciphertext = cipher.doFinal(plaintext);
            byte[] iv = cipher.getIV();

            FileOutputStream output = null;
            try {
                output = profileFile.startWrite();
                DataOutputStream data = new DataOutputStream(output);
                data.writeInt(MAGIC);
                data.writeInt(VERSION);
                data.writeInt(iv.length);
                data.writeInt(ciphertext.length);
                data.write(iv);
                data.write(ciphertext);
                data.flush();
                profileFile.finishWrite(output);
            } catch (IOException error) {
                if (output != null) {
                    profileFile.failWrite(output);
                }
                throw error;
            }
        } catch (GeneralSecurityException | IOException error) {
            throw new P2pVpnException("Failed to encrypt and persist the profile", error);
        }
    }

    synchronized String load() throws P2pVpnException {
        if (!exists()) {
            throw new P2pVpnException("No p2p-vpn profile has been created");
        }
        try (DataInputStream input = new DataInputStream(profileFile.openRead())) {
            if (input.readInt() != MAGIC || input.readInt() != VERSION) {
                throw new P2pVpnException("Stored profile has an unsupported format");
            }
            int ivLength = input.readInt();
            int ciphertextLength = input.readInt();
            if (ivLength < 12 || ivLength > 32) {
                throw new P2pVpnException("Stored profile has an invalid nonce");
            }
            if (ciphertextLength < 16 || ciphertextLength > MAX_PROFILE_BYTES + 32) {
                throw new P2pVpnException("Stored profile has an invalid size");
            }
            byte[] iv = new byte[ivLength];
            byte[] ciphertext = new byte[ciphertextLength];
            input.readFully(iv);
            input.readFully(ciphertext);
            if (input.read() != -1) {
                throw new P2pVpnException("Stored profile contains trailing data");
            }

            Cipher cipher = Cipher.getInstance(CIPHER);
            cipher.init(Cipher.DECRYPT_MODE, profileKey(), new GCMParameterSpec(128, iv));
            cipher.updateAAD(AAD);
            byte[] plaintext = cipher.doFinal(ciphertext);
            return new String(plaintext, StandardCharsets.UTF_8);
        } catch (P2pVpnException error) {
            throw error;
        } catch (GeneralSecurityException | IOException error) {
            throw new P2pVpnException("Failed to decrypt the stored profile", error);
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
