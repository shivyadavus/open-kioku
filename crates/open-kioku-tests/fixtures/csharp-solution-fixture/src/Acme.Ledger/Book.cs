namespace Acme.Ledger;

/// <summary>The entries posted so far.</summary>
public partial class Book : IBook
{
    private readonly List<Entry> entries = new();

    public void Add(Entry entry) => entries.Add(entry);

    public void Add(Entry entry, string note) => Add(entry);
}
