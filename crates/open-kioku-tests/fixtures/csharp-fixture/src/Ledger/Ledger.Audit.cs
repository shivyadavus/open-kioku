namespace Acme.Ledger
{
    partial class Ledger
    {
        internal int Audits { get; private set; }

        public void Audit()
        {
            Audits++;
        }
    }
}
