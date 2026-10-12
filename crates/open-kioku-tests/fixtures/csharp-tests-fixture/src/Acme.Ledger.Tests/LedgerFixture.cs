using Xunit;

namespace Acme.Ledger.Tests;

/// <summary>One ledger shared by every test of a class, set up and torn down once.</summary>
public sealed class LedgerFixture : IAsyncLifetime
{
    public Posting Ledger { get; } = new Posting();

    public Task InitializeAsync()
    {
        Ledger.Post(10m, credit: true);
        return Task.CompletedTask;
    }

    public Task DisposeAsync() => Task.CompletedTask;
}
