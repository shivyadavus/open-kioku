using Xunit;

namespace Acme.Xunit;

public class InMemoryLedgerTests : Acme.Spec.LedgerContractBase
{
}

public class SqlJournalTests : JournalContract
{
}

public partial class JournalContract
{
    [Fact]
    public void Replays() => Assert.True(true);
}

public class SkippedTests
{
    [Fact(Skip = "flaky")]
    public void Skipped() => Assert.True(false);
}
