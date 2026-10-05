using Microsoft.VisualStudio.TestTools.UnitTesting;

namespace Acme.Ledger.MSTest;

[TestClass]
public class VoidingSuite
{
    private Posting posting = new Posting();

    [ClassInitialize]
    public static void OpenLedger(TestContext context)
    {
    }

    [TestInitialize]
    public void Reset()
    {
        posting = new Posting();
    }

    [TestMethod]
    public void VoidCreditReversesIt()
    {
        posting.Post(2m, credit: true);
        posting.Void(2m, credit: true);
        Assert.AreEqual(0m, posting.Balance);
    }

    [DataTestMethod]
    [DataRow(1)]
    [DataRow(2)]
    public void VoidDebitReversesIt(int amount)
    {
        posting.Post(amount, credit: false);
        posting.Void(amount, credit: false);
        Assert.AreEqual(0m, posting.Balance);
    }

    [TestCleanup]
    public void Clean()
    {
    }
}
