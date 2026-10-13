using Acme.Reports;

namespace Acme.Web;

/// <summary>Posts an amount to the ledger book and reports the posting.</summary>
public sealed class PostingEndpoint
{
    private readonly Book book = new();

    public Entry Post(decimal amount, Statement statement)
    {
        var entry = new Entry(amount);
        book.Add(entry);
        return entry;
    }
}
