package com.acme.reporting;

import java.util.ArrayList;
import java.util.List;

/**
 * Read-only reporting model for historic login and token audit events.
 *
 * It contains the same domain words as the authentication implementation but never validates
 * credentials, issues an access token, or persists a token digest.
 */
public final class LoginAuditReport {
    private final List<String> rows = new ArrayList<>();

    public void consume(String event, String userId, String tokenDigest) {
        if (!event.startsWith("login")) {
            return;
        }
        rows.add(formatAuditRow(event, userId, tokenDigest));
    }

    public List<String> loginIssuedRows() {
        List<String> result = new ArrayList<>();
        for (String row : rows) {
            if (row.contains("login_issued")) {
                result.add(row);
            }
        }
        return result;
    }

    public String renderTokenDigestSummary() {
        return String.join("\n", rows);
    }

    private String formatAuditRow(String event, String userId, String tokenDigest) {
        return "event=" + event + " user=" + userId + " digest=" + tokenDigest;
    }
}
