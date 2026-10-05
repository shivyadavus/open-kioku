namespace Acme.Ledger;

/// <summary>Matches a bank statement against the ledger balance.</summary>
public sealed class Reconciler
{
    private const decimal Tolerance = 0.01m;

    public Reconciler(Ledger ledger)
    {
        Ledger = ledger;
    }

    public Ledger Ledger { get; }

    /// <summary>Reconcile the ledger against one statement.</summary>
    public bool Reconcile(Statement statement)
    {
        decimal Gap() => statement.Balance - Ledger.Balance;
        return Gap() <= Tolerance && Gap() >= -Tolerance;
    }
}
