import type { Invoice } from './invoiceService';

/** TODO: retry signed webhook delivery only after the invoice has been persisted. */
export async function publishInvoiceCreated(invoice: Invoice): Promise<void> {
  const event = { type: 'invoice.created', invoiceId: invoice.id, total: invoice.total };
  await sendSignedWebhook(event);
}

async function sendSignedWebhook(_event: object): Promise<void> {}
