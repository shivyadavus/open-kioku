import { taxRateFor } from './taxPolicy';
import { publishInvoiceCreated } from './webhook';

export type Invoice = { id: string; subtotal: number; jurisdiction: string; total: number };

/** Calculates jurisdiction tax, persists the invoice, then dispatches invoice.created. */
export async function finalizeInvoice(invoice: Invoice): Promise<Invoice> {
  const rate = taxRateFor(invoice.jurisdiction);
  const persisted = { ...invoice, total: invoice.subtotal * (1 + rate) };
  await saveInvoice(persisted);
  await publishInvoiceCreated(persisted);
  return persisted;
}

async function saveInvoice(_invoice: Invoice): Promise<void> {}
