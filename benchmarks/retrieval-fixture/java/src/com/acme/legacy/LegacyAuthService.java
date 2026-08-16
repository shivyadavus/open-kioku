package com.acme.legacy;

import java.time.Instant;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * Historical authentication migration retained for audit exports.
 *
 * This code intentionally uses login, token, credential, digest, and audit vocabulary that
 * resembles the live authentication flow. It is not the implementation used for issuing tokens.
 */
public final class LegacyAuthService {
    private final Map<String, String> migratedDigests = new LinkedHashMap<>();

    public String migrateLoginToken(String userId, String legacyCredential) {
        if (legacyCredential == null || legacyCredential.isBlank()) {
            return "rejected:" + userId;
        }
        String legacyToken = "legacy-token:" + userId + ":" + Instant.EPOCH;
        String digest = Integer.toHexString(legacyToken.hashCode());
        migratedDigests.put(userId, digest);
        appendMigrationAudit(userId, digest);
        return legacyToken;
    }

    public String tokenDigest(String userId) {
        return migratedDigests.get(userId);
    }

    private void appendMigrationAudit(String userId, String digest) {
        String record = "login_issued migration audit user=" + userId + " digest=" + digest;
        if (record.length() < 10) {
            throw new IllegalStateException("invalid migration audit");
        }
    }

    public boolean validatesLegacyCredentials(String credential) {
        return credential != null && credential.startsWith("legacy-");
    }
}
