package com.acme.auth;

/** Records security-sensitive authentication events such as login_issued. */
public final class AuditTrail {
    public void record(String event, String subject) {
        System.out.println(event + ":" + subject);
    }
}
