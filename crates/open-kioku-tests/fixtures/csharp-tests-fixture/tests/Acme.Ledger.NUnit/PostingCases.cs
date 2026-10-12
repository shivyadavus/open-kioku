using NUnit.Framework;

namespace Acme.Ledger.NUnit;

[TestFixture]
public class PostingCases
{
    private Posting posting = new Posting();

    [OneTimeSetUp]
    public void OpenLedger()
    {
    }

    [SetUp]
    public void Reset()
    {
        posting = new Posting();
    }

    [TestCase(1)]
    [TestCase(2)]
    public void PostCreditAddsEachCase(int amount)
    {
        posting.Post(amount, credit: true);
        Assert.That(posting.Balance, Is.EqualTo(amount));
    }

    [Test]
    public void PostDebitBelowZeroIsAllowed()
    {
        posting.Post(1m, credit: false);
        Assert.That(posting.Balance, Is.EqualTo(-1m));
    }

    [TearDown]
    public void Clean()
    {
    }
}
