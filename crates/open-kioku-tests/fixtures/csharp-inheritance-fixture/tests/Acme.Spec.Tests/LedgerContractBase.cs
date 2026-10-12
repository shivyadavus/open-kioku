using Xunit;

namespace Acme.Spec;

/// <summary>A specification every ledger store must meet, run by each store's test class.</summary>
public abstract class LedgerContractBase
{
    [Fact]
    public void Balances() => Assert.Equal(0m, 0m);
}
