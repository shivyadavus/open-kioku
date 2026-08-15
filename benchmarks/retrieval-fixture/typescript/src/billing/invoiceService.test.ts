import { finalizeInvoice } from './invoiceService';

/** Tests finalizeInvoice tax calculation for Illinois invoices. */
export async function invoiceTotalIncludesJurisdictionTax() {
  const invoice = await finalizeInvoice({ id: 'i-1', subtotal: 100, jurisdiction: 'IL', total: 0 });
  if (invoice.total <= 100) throw new Error('expected tax');
}
