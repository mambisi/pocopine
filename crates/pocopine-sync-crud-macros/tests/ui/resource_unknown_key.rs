struct Customers;

#[pocopine_sync_crud::resource(stream = "customers")]
impl Customers {}

fn main() {}
