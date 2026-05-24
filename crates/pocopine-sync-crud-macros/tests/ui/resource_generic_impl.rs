pub struct Customer;

pub struct CustomerDraft;

pub struct Customers<T>(T);

#[pocopine_sync_crud::resource(name = "customers")]
impl<T: Send + Sync + 'static> pocopine_sync_crud::CrudSource for Customers<T> {
    type Id = String;
    type Row = Customer;
    type Draft = CustomerDraft;
}

fn main() {}
