namespace Acme.Ledger;

/// <summary>Posts credits and debits against a running balance.</summary>
public sealed class Posting
{
    public decimal Balance { get; private set; }

    public void Post(decimal amount, bool credit)
    {
        Balance += credit ? amount : -amount;
    }

    public void Void(decimal amount, bool credit)
    {
        Post(amount, !credit);
    }
}
