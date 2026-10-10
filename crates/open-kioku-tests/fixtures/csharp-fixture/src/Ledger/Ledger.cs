namespace Acme.Ledger
{
    public partial class Ledger
    {
        private decimal balance;

        public decimal Balance => balance;

        public void Post(decimal amount, EntrySide side)
        {
            balance += side == EntrySide.Credit ? amount : -amount;
        }
    }
}
