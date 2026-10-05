using Xunit;
using Check = Xunit.FactAttribute;

namespace Acme.Ledger.Tests;

public sealed class PostingTests : IClassFixture<LedgerFixture>, IDisposable
{
    private readonly Posting posting;

    public PostingTests(LedgerFixture fixture)
    {
        posting = new Posting();
    }

    public void Dispose()
    {
    }

    [Fact]
    public void PostCreditRaisesTheBalance()
    {
        posting.Post(5m, credit: true);
        Assert.Equal(5m, posting.Balance);
    }

    [Theory]
    [InlineData(1)]
    [InlineData(2)]
    public void PostDebitLowersTheBalance(int amount)
    {
        posting.Post(amount, credit: false);
        Assert.Equal(-amount, posting.Balance);
    }

    [Check]
    public void PostKeepsCreditsAndDebitsApart()
    {
        PostBoth(3m);
        Assert.Equal(0m, posting.Balance);
    }

    private void PostBoth(decimal amount)
    {
        posting.Post(amount, credit: true);
        posting.Post(amount, credit: false);
    }

    public class WhenVoided
    {
        [Fact]
        public void VoidRestoresTheBalance()
        {
            var posting = new Posting();
            posting.Post(4m, credit: true);
            posting.Void(4m, credit: true);
            Assert.Equal(0m, posting.Balance);
        }
    }
}
