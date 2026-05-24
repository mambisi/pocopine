pub struct Customer;

pub struct Customers;

#[pocopine_sync_crud::resource(name = "customers")]
impl pocopine_sync_crud::CrudSource for Customers {
    type Id = String;
    type Row = Customer;
}

fn main() {}
