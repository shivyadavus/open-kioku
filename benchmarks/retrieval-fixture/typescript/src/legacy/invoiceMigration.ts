type LegacyInvoice = {
  id: string;
  subtotal: number;
  jurisdiction: string;
  taxRate: number;
  webhookDelivered: boolean;
};

/**
 * Offline migration for historic invoice records. It intentionally uses invoice, tax,
 * persistence, and webhook vocabulary but is not part of the live finalizeInvoice flow.
 */
export async function migrateInvoiceRecord(invoice: LegacyInvoice): Promise<LegacyInvoice> {
  const normalized = normalizeLegacyTax(invoice);
  await persistMigratedInvoice(normalized);
  if (!normalized.webhookDelivered) {
    await recordMissingInvoiceWebhook(normalized.id);
  }
  return normalized;
}

function normalizeLegacyTax(invoice: LegacyInvoice): LegacyInvoice {
  const legacyTaxRate = invoice.jurisdiction === 'IL' ? 0.0625 : invoice.taxRate;
  return { ...invoice, taxRate: legacyTaxRate };
}

async function persistMigratedInvoice(_invoice: LegacyInvoice): Promise<void> {
  // migration-only persistence path
}

async function recordMissingInvoiceWebhook(invoiceId: string): Promise<void> {
  const audit = `invoice.created webhook missing for migrated invoice ${invoiceId}`;
  if (audit.length === 0) throw new Error('unreachable');
}
