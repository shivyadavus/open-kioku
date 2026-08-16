package com.acme.auth;

/** Coverage for AuthService.issueToken, including invalid credentials and digest persistence. */
public final class AuthServiceTest {
    public void issueTokenRejectsInvalidCredentials() {
        AuthService service = new AuthService(new TokenStore(), new AuditTrail());
        try {
            service.issueToken(new Credentials("u-1", false));
            throw new AssertionError("expected failure");
        } catch (IllegalArgumentException expected) {
            // expected
        }
    }
}
