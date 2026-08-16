package com.acme.auth;

public final class AuthService {
    private final TokenStore tokenStore;
    private final AuditTrail auditTrail;

    public AuthService(TokenStore tokenStore, AuditTrail auditTrail) {
        this.tokenStore = tokenStore;
        this.auditTrail = auditTrail;
    }

    /** Login flow: validate credentials, issue an access token, persist its digest, then audit login_issued. */
    public String issueToken(Credentials credentials) {
        if (!credentials.valid()) {
            throw new IllegalArgumentException("invalid credentials");
        }
        String token = TokenSigner.sign(credentials.userId());
        tokenStore.saveDigest(credentials.userId(), TokenSigner.digest(token));
        auditTrail.record("login_issued", credentials.userId());
        return token;
    }
}

record Credentials(String userId, boolean valid) {}
final class TokenSigner {
    static String sign(String userId) { return "token:" + userId; }
    static String digest(String token) { return Integer.toHexString(token.hashCode()); }
}
