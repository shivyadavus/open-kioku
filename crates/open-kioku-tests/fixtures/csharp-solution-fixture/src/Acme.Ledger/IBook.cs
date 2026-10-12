namespace Acme.Ledger;

/// <summary>What a ledger book accepts.</summary>
public interface IBook
{
    void Add(Entry entry);
}
