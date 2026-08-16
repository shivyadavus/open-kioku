package com.acme.auth;

import java.util.HashMap;
import java.util.Map;

/** Persists access-token digests keyed by user id. */
public final class TokenStore {
    private final Map<String, String> digests = new HashMap<>();

    public void saveDigest(String userId, String digest) {
        digests.put(userId, digest);
    }

    public String digestFor(String userId) {
        return digests.get(userId);
    }
}
