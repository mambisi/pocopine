struct Customers;

#[pocopine_sync_crud::resource(name = "customers", schema_version = 0)]
impl Customers {}

fn main() {}
