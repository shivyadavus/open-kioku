export type InvoiceAuditRow = {
  invoiceId: string;
  jurisdiction: string;
  taxRate: number;
  webhookEvent: string;
  total: number;
};

/** Read-only reporting over persisted invoice tax and webhook history. */
export class InvoiceAuditReport {
  private readonly rows: InvoiceAuditRow[] = [];

  ingest(row: InvoiceAuditRow): void {
    this.rows.push(row);
  }

  invoiceCreatedEvents(): InvoiceAuditRow[] {
    return this.rows.filter((row) => row.webhookEvent === 'invoice.created');
  }

  jurisdictionTaxSummary(jurisdiction: string): number {
    return this.rows
      .filter((row) => row.jurisdiction === jurisdiction)
      .reduce((total, row) => total + row.taxRate, 0);
  }

  persistedTotals(): number[] {
    return this.rows.map((row) => row.total);
  }

  webhookDeliverySummary(): string {
    return this.rows
      .map((row) => `${row.invoiceId}:${row.webhookEvent}:${row.total}`)
      .join('\n');
  }
}
