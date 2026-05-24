struct Customers;

#[pocopine_sync_crud::resource(name = "customers", unexpected)]
impl Customers {}

fn main() {}
