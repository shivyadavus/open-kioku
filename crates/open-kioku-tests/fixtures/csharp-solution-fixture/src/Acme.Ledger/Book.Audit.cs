namespace Acme.Ledger;

public partial class Book
{
    public int Audited { get; private set; }

    public void Audit() => Audited++;
}
